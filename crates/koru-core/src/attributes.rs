// SPDX-License-Identifier: MPL-2.0

//! The attribute registry, and the validated attribute operations.
//!
//! **Four attribute classes, one engine, not one storage model** ([ADR-176],
//! roadmap D1): *system semantic* attributes Koru understands (`koru.tag`,
//! `koru.color`, `koru.due`, `koru.priority`, and the reserved personal
//! `koru.plan`); *custom* attributes a workspace defines, typed but
//! semantically generic; *derived* metadata answered from the store, never
//! stored; and dedicated domain models that only look like attributes.
//!
//! Values live on the node's own `attrs` map — the definition's key is the
//! attr's `role` — so they move with the node, merge per entry, and are
//! already projected. Definitions for custom attributes live in the workspace
//! system document ([`crate::system::SystemDoc`]); the system ones live here,
//! in code, because their semantics are this build's.
//!
//! **Identity never depends on display names.** A system attribute's identity
//! is its namespaced key; a custom one's is its generated id; a tag's is its
//! [`TagId`](crate::ids::TagId). Rename touches a label and rewrites nothing.
//!
//! [ADR-176]: ../../../docs/decisions/ADR-176-200.md#adr-199-attributes--attributes-are-typed-values-on-the-node-under-registered-keys-tags-are-entities-and-the-workspace-keeps-a-hidden-system-document

use crate::ids::{AttrId, ListId, NodeId, TagId};
use crate::node::{Attr, AttrValue};
use crate::schema::{Result, StoreError, WorkspaceDoc};

/// Tags. Multi-valued: one attr per tag, each value the [`TagId`] as text.
/// Names and colors live in the system document's tag table, never on nodes.
///
/// [`TagId`]: crate::ids::TagId
pub const KEY_TAG: &str = "koru.tag";
/// Which **list** this task is in — one [`ListId`] as text, or absence.
///
/// **A list is a filter, not a place** (SPEC §8): membership is this one value,
/// so "the Bike Project list" is *select tasks where `koru.list` = that id*,
/// and a task can sit in Today *and* in Bike Project without two vocabularies.
/// Names, colors and rail order live in the workspace system document's list
/// table, exactly as a tag's do — so a rename touches one record and no task.
///
/// **Cardinality one**, unlike [`KEY_TAG`]: a task is in one list or in none.
/// The multi-valued question is what tags already answer, and a second
/// many-valued vocabulary would be one classification system too many. It is an
/// **ordinary** attribute — not [command-only](COMMAND_ONLY_KEYS) — so a list
/// change goes through the generic doors, carries no lifecycle invariant, and
/// emits **no** task activity this release, exactly like a tag (SPEC §9).
///
/// [`ListId`]: crate::ids::ListId
pub const KEY_LIST: &str = "koru.list";
/// An accent color token from [`AccentColor`]. Presentation, never meaning
/// ([Foundation invariant 13]): nothing may infer "red = urgent". Distinct
/// from the legacy `NodeMeta.colour`, which stays untouched.
///
/// [Foundation invariant 13]: ../../../docs/decisions/ADR-176-200.md#adr-199-attributes--attributes-are-typed-values-on-the-node-under-registered-keys-tags-are-entities-and-the-workspace-keeps-a-hidden-system-document
pub const KEY_COLOR: &str = "koru.color";
/// When this must be completed. **Shared**, so a timed due is a
/// [`ZonedDateTime`] — collaborators must agree on the same moment, and a
/// deadline must not move when somebody flies ([ADR-183]). A day with no time
/// stays a [`DateOnly`]. Derived states — overdue, due today — are computed,
/// never stored.
///
/// [`ZonedDateTime`]: crate::time::ZonedDateTime
/// [`DateOnly`]: crate::time::DateOnly
/// [ADR-183]: ../../../docs/decisions/ADR-201-225.md#adr-206-timemodel--koru-has-three-temporal-types-not-one-with-a-nullable-timezone
pub const KEY_DUE: &str = "koru.due";
/// How important this is: one of [`Priority`]'s four tokens, unset as absence.
pub const KEY_PRIORITY: &str = "koru.priority";
/// **Where the task sits in the order somebody dragged it into**: an integer,
/// smaller first. One order per task, read by every page ordered by hand — a
/// list, and Today, Next, Upcoming or Later set to *Manual* — so a task moved
/// up on one is moved up among the same tasks on the others. Absent on a task
/// nobody has dragged, which such a page orders after the ranked ones by its
/// due. Presentation of an order, never meaning, and never drawn as a chip —
/// the order *is* how it is shown.
pub const KEY_RANK: &str = "koru.rank";
/// The pre-registry bare role older documents wrote a due under, before
/// [`KEY_DUE`] existed. Read everywhere a due is read ([`crate::node::Node::due`]),
/// which is what makes a legacy due-dated row a task with no migration.
pub const LEGACY_DUE_ROLE: &str = "due";

/// Whether `key` names a due — the registered key or the legacy bare role.
pub fn is_due_key(key: &str) -> bool {
    key == KEY_DUE || key == LEGACY_DUE_ROLE
}

/// The role a **delegated run**'s pill is stored under
/// (`docs/architecture/DATAMODEL.md`; ADR-336).
///
/// **This is a stored name and it never changes.** It is the literal text in
/// the `role` key of an attribute record in every document that carries a run
/// pill, on every device, in every replica and in every export. Renaming it
/// would orphan every pill already written. The same string is spelled by the
/// phone's reader (`koru-mobile-host`'s `RUN_PILL_ROLE`) and pinned by
/// `koru-core/tests/run_pill.rs`.
///
/// It is deliberately **not** a [`system_definitions`] entry: a run pill is a
/// reference a gesture left behind, not an attribute the attribute surfaces
/// offer to set. `Attr.role` is open text, which is what lets the pill ride
/// the existing `"ref"` storage token with no content-schema bump.
pub const ROLE_RUN: &str = "koru.run";

/// The role a recurring delegation schedule's inline pill is stored under.
///
/// The value is the schedule's stable storage id as text. This is an open
/// attribute role, like [`ROLE_RUN`], rather than a registered attribute a
/// person can add from the properties surface. Older builds preserve the
/// record and draw its stored label without needing a content-schema bump.
pub const ROLE_SCHEDULE: &str = "koru.schedule";

/// The role of a **quote chip in the AI chat's composer** (koru-ai D88): a
/// passage of a reply, held whole as text, drawn as one chip and sent as a
/// Markdown blockquote (`koru_export_markdown::message_text`).
///
/// The composer is a private in-memory document that is never stored, so
/// this name is not written anywhere a later build reads it back. It is an
/// open role like [`ROLE_RUN`] — a [`Text`](crate::node::AttrValue::Text)
/// record, no new value type and no content-schema bump — and it is named
/// here only so the editor that draws the chip and the export that sends it
/// spell it the same.
pub const ROLE_QUOTE: &str = "koru.quote";

/// The role of a **skill chip in the AI chat's composer** (koru-ai D121): a
/// skill of the chat's agent picked under `$`, held as its name — a
/// [`Text`](crate::node::AttrValue::Text) record whose label is what the
/// chip shows — and sent as `$name` (`koru_export_markdown::message_text`).
///
/// Like [`ROLE_QUOTE`] it lives only in the composer's private in-memory
/// document, so nothing stores it; it is named here so the editor that draws
/// the chip, the export that writes it and the chat that sends its name
/// spell it the same.
pub const ROLE_SKILL: &str = "koru.skill";

/// A selected agent command in the chat composer's private document. Like a
/// skill chip, its stored message is a durable link; only provider prompt
/// rendering turns it into `/name` at its selected position.
pub const ROLE_COMMAND: &str = "koru.command";

/// The roles whose `"ref"` record means **a reference to a document**, and
/// which are therefore read back as [`AttrValue::Document`] rather than as a
/// bare [`AttrValue::Ref`] homed at one.
///
/// Storage is identical for every role: `value` is the document's uuid,
/// `home` is `document:<uuid>`, and `target` carries a deep node when there is
/// one. What this set decides is only **what the reader hands back**, so
/// widening it is a code change and never a storage change — a build that does
/// not know a role in this list still reads, preserves and re-exports the
/// record byte for byte, and draws the pill from its stored label.
///
/// [`AttrValue::Document`]: crate::node::AttrValue::Document
/// [`AttrValue::Ref`]: crate::node::AttrValue::Ref
pub const DOCUMENT_REFERENCE_ROLES: [&str; 2] = ["document", ROLE_RUN];

/// Whether a `"ref"` record under this role reads back as a typed document
/// reference. See [`DOCUMENT_REFERENCE_ROLES`].
pub fn is_document_reference_role(role: &str) -> bool {
    DOCUMENT_REFERENCE_ROLES.contains(&role)
}

/// When one person intends to work on this. **Personal** (D4): the value lives
/// in per-profile storage, never on the shared node — the definition is here
/// so the key is reserved and typed, and [`WorkspaceDoc::set_attribute`]
/// refuses it. Storage lands with plan A1b; no entry UI in R0.
///
/// A timed plan is a [`FloatingDateTime`]: "work on this at 09:00" follows the
/// person, so it must not be pinned to the place they were sitting when they
/// typed it ([ADR-183]).
///
/// [`FloatingDateTime`]: crate::time::FloatingDateTime
pub const KEY_PLAN: &str = "koru.plan";

/// Whether this node is a task, and how it became one — one of
/// [`TaskReason`]'s three tokens.
///
/// **Command-only**: the generic attribute doors refuse it (see
/// [`COMMAND_ONLY_KEYS`]), because the invariants around it — the
/// `NodeMeta::done` mirror, the completion cascade, the reopen
/// classification — live in [`crate::tasks`] rather than in a value check, and
/// a raw write would be a second authority on the same question ([ADR-285]).
///
/// [ADR-285]: ../../../docs/decisions/ADR-276-300.md#adr-285-taskmodel--a-task-is-a-marked-node-and-the-marker-or-a-due-date-is-the-whole-derivation
pub const KEY_TASK: &str = "koru.task";
/// The canonical task lifecycle. **Absence means `open`**; `completed` is the
/// only value that means completed, and it is stored independently of how many
/// children the task has. **Command-only**, for [`KEY_TASK`]'s reason.
pub const KEY_TASK_STATE: &str = "koru.task_state";
/// **Pinned to Today by hand.** Present means somebody put this task on the
/// Today queue themselves, whatever its due date says.
///
/// It is a *membership*, not a date and not a relocation: a task due Friday and
/// pinned is on Today **and** still on Upcoming, because the pin says "I am
/// working on this today" and the due still says when it is owed. Nothing here
/// derives one from the other, and [`crate::tasks::bucket`] is untouched by it
/// — the natural bucket is still the one the due, the lifecycle and the
/// priority name.
///
/// **Command-only**, for [`KEY_TASK`]'s reason: which queue a task appears on
/// is a classification, and a raw write would be a second authority on it.
pub const KEY_TODAY: &str = "koru.today";

/// Keys the generic attribute doors refuse: [`WorkspaceDoc::set_attribute`],
/// [`WorkspaceDoc::set_attribute_defined`], [`WorkspaceDoc::clear_attribute`],
/// the `*_on_all` family, and MCP's `SetAttribute`/`ClearAttribute` arms.
///
/// The commands write *below* that line, through `set_one_attr_raw` and
/// `remove_attr`, which is why the guard does not have to know about them. The
/// two keys are still **registered definitions**, so they appear in the
/// attribute query's `definitions` list and are filterable and groupable —
/// which is what the read path wants; the editor's `/attr` chooser filters this
/// constant rather than the registry gaining a field ([ADR-286]).
///
/// [ADR-286]: ../../../docs/decisions/ADR-276-300.md#adr-286-taskboundary--six-canonical-commands-own-task-state-and-every-host-is-a-thin-wrapper-around-them
pub const COMMAND_ONLY_KEYS: &[&str] = &[KEY_TASK, KEY_TASK_STATE, KEY_COMPLETED_AT, KEY_TODAY];

/// **The day a task was finished on.** Written by `complete_task`, cleared by
/// `reopen_task` and by `remove_from_tasks`, and command-only for the reason
/// the other two task keys are: it is lifecycle state, and a generic
/// attribute door writing it would make the record disagree with the
/// lifecycle beside it.
///
/// **A day, not an instant.** What it is for is *when did I finish this*, at
/// the resolution a person asks it in — the Completed queue shows a window of
/// recent days, and a chip beside a finished task reads *Sep 5*. A day
/// compares as canonical `YYYY-MM-DD` text, in date order, through the same
/// index a date-kinded due already uses; an instant would need the zoned
/// arm's numeric half for a question nobody asks in milliseconds.
///
/// **The caller says which day**, because *today* is a zone question and this
/// crate carries no timezone database — the rule `BucketWindows` already
/// states from the other side.
pub const KEY_COMPLETED_AT: &str = "koru.completed_at";

/// How a node became a task. **Recorded, never inferred** — a due-derived task
/// and one somebody typed `/task` on look identical afterwards, and the
/// difference is worth an attribute.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub enum TaskReason {
    Explicit,
    Due,
    TasksView,
}

impl TaskReason {
    pub const ALL: [Self; 3] = [Self::Explicit, Self::Due, Self::TasksView];

    /// The stable storage token.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Explicit => "explicit",
            Self::Due => "due",
            Self::TasksView => "tasks_view",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|r| r.as_str() == s)
    }
}

/// The canonical lifecycle. [`Open`](Self::Open) is the default because
/// **absence is `open`**: a due-derived task is open without anybody writing a
/// value.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Default, Debug)]
pub enum Lifecycle {
    #[default]
    Open,
    Completed,
}

impl Lifecycle {
    pub const ALL: [Self; 2] = [Self::Open, Self::Completed];

    /// The stable storage token.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Completed => "completed",
        }
    }

    /// A token back to its value. **Anything else — absence, or a token from a
    /// newer build — reads as [`Open`](Self::Open)** wherever a lifecycle is
    /// parsed: a task this build cannot classify is a task it still shows.
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|l| l.as_str() == s)
    }
}

/// The value type a definition declares. What Koru *understands* about a
/// custom attribute — the business meaning stays the workspace's own.
///
/// `person` and `relation` are reserved by the roadmap and deliberately not
/// variants yet: an unknown type name parses as `None`, so a definition from
/// a newer build is skipped rather than misread.
///
/// **A definition declares which temporal type its key takes**, and there is
/// no "datetime" that means either ([ADR-183]): a key is a day, a shared
/// moment, or a personal wall clock, and validation refuses the other shapes.
/// The retired `datetime` token — the one that meant instant-plus-optional-zone
/// — parses as `None` like any stranger.
///
/// [ADR-183]: ../../../docs/decisions/ADR-201-225.md#adr-206-timemodel--koru-has-three-temporal-types-not-one-with-a-nullable-timezone
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum ValueType {
    Text,
    Number,
    Boolean,
    /// A calendar date, and only that — never converted through UTC.
    Date,
    /// A **shared** timed value: a [`ZonedDateTime`], or a plain
    /// [`DateOnly`] for a day with no time — "due August 30" and "due August
    /// 30, 5 pm" are one attribute. A floating wall clock is refused: a
    /// deadline everybody has to agree on cannot mean a different moment on
    /// each collaborator's desk.
    ///
    /// [`ZonedDateTime`]: crate::time::ZonedDateTime
    /// [`DateOnly`]: crate::time::DateOnly
    ZonedDateTime,
    /// A **personal** timed value: a [`FloatingDateTime`], or a plain
    /// [`DateOnly`]. A zoned value is refused — personal time follows the
    /// person, and pinning it to a place is a decision nobody made.
    ///
    /// [`FloatingDateTime`]: crate::time::FloatingDateTime
    /// [`DateOnly`]: crate::time::DateOnly
    FloatingDateTime,
    Select,
    MultiSelect,
}

impl ValueType {
    pub const ALL: [Self; 8] = [
        Self::Text,
        Self::Number,
        Self::Boolean,
        Self::Date,
        Self::ZonedDateTime,
        Self::FloatingDateTime,
        Self::Select,
        Self::MultiSelect,
    ];

    /// The stable storage token. The temporal three match
    /// [`TemporalKind::token`](crate::time::TemporalKind::token), so a
    /// definition and a value spell the same distinction the same way.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Number => "number",
            Self::Boolean => "boolean",
            Self::Date => crate::time::TemporalKind::Date.token(),
            Self::ZonedDateTime => crate::time::TemporalKind::Zoned.token(),
            Self::FloatingDateTime => crate::time::TemporalKind::Floating.token(),
            Self::Select => "select",
            Self::MultiSelect => "multi_select",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|t| t.as_str() == s)
    }

    /// Whether this type takes a temporal value at all — the three date-ish
    /// members, and nothing else.
    pub fn is_temporal(self) -> bool {
        matches!(
            self,
            Self::Date | Self::ZonedDateTime | Self::FloatingDateTime
        )
    }
}

/// How many values one entity may carry under one key.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Cardinality {
    One,
    Many,
}

impl Cardinality {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::One => "one",
            Self::Many => "many",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "one" => Some(Self::One),
            "many" => Some(Self::Many),
            _ => None,
        }
    }
}

/// Shared vs personal is a **storage-level** distinction, not a display flag
/// (D4, Foundation invariant 6). Shared values live on the node and sync with
/// the document; personal values live per profile and never overwrite another
/// collaborator's.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum AttributeScope {
    Shared,
    Personal,
}

impl AttributeScope {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Shared => "shared",
            Self::Personal => "personal",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "shared" => Some(Self::Shared),
            "personal" => Some(Self::Personal),
            _ => None,
        }
    }
}

/// Fixed semantic importance (D6): four levels, unset as absence — never a
/// fifth token, and not workspace-redefinable. Priority is *importance*;
/// order within a collection is Rank, a different concept that never lives on
/// the entity (Foundation invariants 14–17).
///
/// `Ord` follows declaration order — `Low < Medium < High < Urgent` — so
/// "priority ≥ high" is a comparison. The semantic *sort* for views is
/// descending importance with unset last; that is a view's ordering, not this
/// enum's.
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum Priority {
    Low,
    Medium,
    High,
    Urgent,
}

impl Priority {
    pub const ALL: [Self; 4] = [Self::Low, Self::Medium, Self::High, Self::Urgent];

    /// The stable storage token.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Urgent => "urgent",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.as_str() == s)
    }
}

/// The named accent set `koru.color` draws from. **Tokens, not hex**: what a
/// token looks like belongs to the theme, per theme, so an accent stays
/// legible in dark mode and can be retuned without touching a document.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub enum AccentColor {
    Red,
    Orange,
    Yellow,
    Green,
    Teal,
    Blue,
    Purple,
    Pink,
    Gray,
}

impl AccentColor {
    pub const ALL: [Self; 9] = [
        Self::Red,
        Self::Orange,
        Self::Yellow,
        Self::Green,
        Self::Teal,
        Self::Blue,
        Self::Purple,
        Self::Pink,
        Self::Gray,
    ];

    /// The stable storage token.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Red => "red",
            Self::Orange => "orange",
            Self::Yellow => "yellow",
            Self::Green => "green",
            Self::Teal => "teal",
            Self::Blue => "blue",
            Self::Purple => "purple",
            Self::Pink => "pink",
            Self::Gray => "gray",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|c| c.as_str() == s)
    }
}

/// What the registry knows about one attribute — key and type, plus the
/// capabilities a filter UI builds from (D2).
///
/// `id` is the stable identity: the namespaced key itself for a system
/// attribute, a generated UUID string for a custom one
/// ([`AttributeDefinition::custom`] mints it). `label` is display and may be
/// renamed freely; the id never changes (Foundation invariant 19).
#[derive(Clone, PartialEq, Debug)]
pub struct AttributeDefinition {
    pub id: String,
    /// The attr `role` values are stored under.
    pub key: String,
    pub label: String,
    pub value_type: ValueType,
    pub cardinality: Cardinality,
    pub scope: AttributeScope,
    /// Declared by Koru, with stable semantics — as opposed to workspace-made.
    pub system: bool,
    /// The legal tokens for `select`/`multi_select`. Empty for other types —
    /// and empty for `koru.tag`, whose "options" are the workspace's tag
    /// table, validated as [`TagId`]s instead.
    ///
    /// [`TagId`]: crate::ids::TagId
    pub options: Vec<String>,
    pub filterable: bool,
    pub sortable: bool,
    pub groupable: bool,
}

impl AttributeDefinition {
    /// A workspace-defined attribute, with a freshly minted stable id.
    ///
    /// The id is a v4 UUID, never derived from the key or the label —
    /// rename must not change identity (ADR-39's rule, applied here).
    pub fn custom(key: impl Into<String>, label: impl Into<String>, value_type: ValueType) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            key: key.into(),
            label: label.into(),
            value_type,
            cardinality: match value_type {
                ValueType::MultiSelect => Cardinality::Many,
                _ => Cardinality::One,
            },
            scope: AttributeScope::Shared,
            system: false,
            options: Vec::new(),
            filterable: true,
            sortable: value_type.is_temporal()
                || matches!(value_type, ValueType::Number | ValueType::Text),
            groupable: matches!(value_type, ValueType::Select | ValueType::MultiSelect),
        }
    }

    /// The tokens a select accepts. Builder-style, for custom definitions.
    pub fn with_options(mut self, options: Vec<String>) -> Self {
        self.options = options;
        self
    }

    fn system(
        key: &str,
        label: &str,
        value_type: ValueType,
        cardinality: Cardinality,
        scope: AttributeScope,
        options: Vec<String>,
        (filterable, sortable, groupable): (bool, bool, bool),
    ) -> Self {
        Self {
            id: key.to_owned(),
            key: key.to_owned(),
            label: label.to_owned(),
            value_type,
            cardinality,
            scope,
            system: true,
            options,
            filterable,
            sortable,
            groupable,
        }
    }
}

/// Every system attribute this build understands, in registry order.
pub fn system_definitions() -> Vec<AttributeDefinition> {
    vec![
        AttributeDefinition::system(
            KEY_TAG,
            "Tags",
            ValueType::MultiSelect,
            Cardinality::Many,
            AttributeScope::Shared,
            Vec::new(),
            (true, false, true),
        ),
        // **Groupable and filterable, never sortable.** A list is a bucket, and
        // ordering tasks by the id of the bucket they are in is an order
        // nobody asked for. Its `options` are empty for [`KEY_TAG`]'s reason:
        // the legal values are the workspace's list table, validated as
        // [`ListId`]s rather than enumerated here.
        AttributeDefinition::system(
            KEY_LIST,
            "List",
            ValueType::Select,
            Cardinality::One,
            AttributeScope::Shared,
            Vec::new(),
            (true, false, true),
        ),
        AttributeDefinition::system(
            KEY_COLOR,
            "Color",
            ValueType::Select,
            Cardinality::One,
            AttributeScope::Shared,
            AccentColor::ALL
                .iter()
                .map(|c| c.as_str().to_owned())
                .collect(),
            (true, false, true),
        ),
        AttributeDefinition::system(
            KEY_DUE,
            "Due",
            ValueType::ZonedDateTime,
            Cardinality::One,
            AttributeScope::Shared,
            Vec::new(),
            (true, true, false),
        ),
        AttributeDefinition::system(
            KEY_PRIORITY,
            "Priority",
            ValueType::Select,
            Cardinality::One,
            AttributeScope::Shared,
            Priority::ALL
                .iter()
                .map(|p| p.as_str().to_owned())
                .collect(),
            (true, true, true),
        ),
        // **Sortable and nothing else**: a rank is an order, and a filter or a
        // group over one would be a question nobody asks.
        AttributeDefinition::system(
            KEY_RANK,
            "Rank",
            ValueType::Number,
            Cardinality::One,
            AttributeScope::Shared,
            Vec::new(),
            (false, true, false),
        ),
        // Neither task key is **sortable**: a creation reason and a two-state
        // lifecycle have no order, and a sort control offering one would be a
        // lie. Both are groupable and filterable, which is what the read path
        // needs (ADR-274).
        AttributeDefinition::system(
            KEY_TASK,
            "Task",
            ValueType::Select,
            Cardinality::One,
            AttributeScope::Shared,
            TaskReason::ALL
                .iter()
                .map(|r| r.as_str().to_owned())
                .collect(),
            (true, false, true),
        ),
        AttributeDefinition::system(
            KEY_TASK_STATE,
            "Task state",
            ValueType::Select,
            Cardinality::One,
            AttributeScope::Shared,
            Lifecycle::ALL
                .iter()
                .map(|l| l.as_str().to_owned())
                .collect(),
            (true, false, true),
        ),
        // Filterable and sortable, never groupable: *finished on the 5th* is
        // a window and an order, and a group per day would be a heading over
        // one row each.
        AttributeDefinition::system(
            KEY_COMPLETED_AT,
            "Completed on",
            ValueType::Date,
            Cardinality::One,
            AttributeScope::Shared,
            Vec::new(),
            (true, true, false),
        ),
        // **Shared, not personal — and that is a statement about today rather
        // than about the idea.** "I am working on this today" is a personal
        // fact, and [`KEY_PLAN`] is the key reserved for exactly that. Its
        // per-profile storage is not built (plan A1b), so a definition claiming
        // `Personal` here would be a label the storage does not honour: the
        // value would go on the shared node like every other. It is `Shared`
        // because that is where it lands. When personal storage arrives, this
        // and `koru.plan` should be looked at together rather than left as two
        // answers to one question.
        //
        // Filterable and groupable, never sortable, like the other
        // command-only keys: an order over a membership would be two heaps.
        AttributeDefinition::system(
            KEY_TODAY,
            "On Today",
            ValueType::Boolean,
            Cardinality::One,
            AttributeScope::Shared,
            Vec::new(),
            (true, false, true),
        ),
        AttributeDefinition::system(
            KEY_PLAN,
            "Plan",
            ValueType::FloatingDateTime,
            Cardinality::One,
            AttributeScope::Personal,
            Vec::new(),
            (true, true, false),
        ),
    ]
}

/// The system definition behind `key`, if it is one.
pub fn system_definition(key: &str) -> Option<AttributeDefinition> {
    system_definitions().into_iter().find(|d| d.key == key)
}

/// Whether `value` is legal under `def`. `Err` carries the reason, phrased for
/// an error message — never the value itself, which may be user content.
pub fn validate(def: &AttributeDefinition, value: &AttrValue) -> std::result::Result<(), String> {
    // **The temporal types do not substitute for one another.** A shared key
    // refusing a floating value, and a personal key refusing a zoned one, is
    // what stops "no implicit conversion" (invariant 23) from being a
    // convention nobody enforces: there is no code path that quietly re-reads
    // one as the other, because the write never lands.
    let type_ok = match def.value_type {
        ValueType::Text => matches!(value, AttrValue::Text(_)),
        ValueType::Number => matches!(value, AttrValue::Number(_)),
        ValueType::Boolean => matches!(value, AttrValue::Bool(_)),
        ValueType::Date => matches!(value, AttrValue::Date(_)),
        // A timed attribute accepts a calendar date too: a day with no time is
        // the common case, and it stays a date rather than faking midnight.
        ValueType::ZonedDateTime => {
            if matches!(value, AttrValue::FloatingDateTime(_)) {
                return Err(
                    "is shared, so a timed value carries the zone it was meant in; \
                     this is a floating wall clock"
                        .into(),
                );
            }
            matches!(value, AttrValue::Date(_) | AttrValue::ZonedDateTime(_))
        }
        ValueType::FloatingDateTime => {
            if matches!(value, AttrValue::ZonedDateTime(_)) {
                return Err("is personal, so a timed value is a floating wall clock; \
                     this one is pinned to a timezone"
                    .into());
            }
            matches!(value, AttrValue::Date(_) | AttrValue::FloatingDateTime(_))
        }
        ValueType::Select | ValueType::MultiSelect => matches!(value, AttrValue::Text(_)),
    };
    if !type_ok {
        return Err(format!("expects a {} value", def.value_type.as_str()));
    }
    if def.key == KEY_TAG {
        // A tag value is a TagId, not free text — the tag table owns names.
        match value {
            AttrValue::Text(t) if TagId::parse(t).is_some() => {}
            _ => return Err("expects a tag id".into()),
        }
        return Ok(());
    }
    if def.key == KEY_LIST {
        // A membership is a ListId, not a name — the list table owns names, and
        // a stored name would have to be rewritten by every rename. An id the
        // table has forgotten simply resolves to no list, which is what makes
        // deleting a list cost nothing node-side.
        match value {
            AttrValue::Text(t) if ListId::parse(t).is_some() => {}
            _ => return Err("expects a list id".into()),
        }
        return Ok(());
    }
    // The two command-only keys are validated here as well as guarded at the
    // doors, so the registry stays the one authority on what a task marker or a
    // lifecycle may say: the commands write below the doors and call this
    // before they do (ADR-272's cascade preflight, step 10).
    if def.key == KEY_TASK {
        match value {
            AttrValue::Text(token) if TaskReason::parse(token).is_some() => return Ok(()),
            _ => return Err("expects a task creation reason".into()),
        }
    }
    if def.key == KEY_TASK_STATE {
        match value {
            AttrValue::Text(token) if Lifecycle::parse(token).is_some() => return Ok(()),
            _ => return Err("expects a task lifecycle".into()),
        }
    }
    if matches!(def.value_type, ValueType::Select | ValueType::MultiSelect)
        && !def.options.is_empty()
    {
        if let AttrValue::Text(token) = value {
            if !def.options.iter().any(|o| o == token) {
                return Err("is not one of the definition's options".into());
            }
        }
    }
    // A zone needs no check here: `ZonedDateTime` cannot exist without a
    // `TimeZoneName`, and that type refuses offsets at construction. The
    // invalid state is unrepresentable rather than validated away.
    Ok(())
}

/// The refusal both command-only guards answer with. One function so the
/// generic doors and the clearing doors say the same thing, and a test can
/// assert on one sentence.
fn command_only(key: &str) -> StoreError {
    StoreError::AttributeRejected {
        key: key.to_owned(),
        why: "is written by the task commands, not by a generic attribute door".into(),
    }
}

/// Whether `key` is one the generic doors refuse — [`COMMAND_ONLY_KEYS`], and
/// nothing else. Public because two readers ask it: these doors, and the
/// editor's `/attr` chooser, which filters the same constant rather than the
/// registry gaining a field.
pub fn is_command_only_key(key: &str) -> bool {
    COMMAND_ONLY_KEYS.contains(&key)
}

/// The shared front gate of every set: command-only keys refused ([ADR-286]),
/// personal scope refused (D4), value validated against the definition.
///
/// [ADR-286]: ../../../docs/decisions/ADR-276-300.md#adr-286-taskboundary--six-canonical-commands-own-task-state-and-every-host-is-a-thin-wrapper-around-them
fn check_settable(def: &AttributeDefinition, value: &AttrValue) -> Result<()> {
    if is_command_only_key(&def.key) {
        return Err(command_only(&def.key));
    }
    if def.scope == AttributeScope::Personal {
        return Err(StoreError::AttributeRejected {
            key: def.key.clone(),
            why: "is personal and does not live on a shared node".into(),
        });
    }
    validate(def, value).map_err(|why| StoreError::AttributeRejected {
        key: def.key.clone(),
        why,
    })
}

impl WorkspaceDoc {
    /// Set a **system** attribute on a node, validated against the registry.
    ///
    /// Idempotent-safe: setting the value already there changes nothing and
    /// leaves no undo step. For a cardinality-one key the existing record is
    /// updated in place (same [`AttrId`]), and any convergence duplicates —
    /// two peers first-setting the key concurrently each mint a record — are
    /// folded into the lowest id, the `ensure_trash_root` idiom at attr level.
    ///
    /// A custom attribute goes through
    /// [`set_attribute_defined`](Self::set_attribute_defined) with its
    /// definition from the workspace system document.
    pub fn set_attribute(&mut self, node: NodeId, key: &str, value: AttrValue) -> Result<AttrId> {
        let def =
            system_definition(key).ok_or_else(|| StoreError::UnknownAttribute(key.to_owned()))?;
        self.set_attribute_defined(node, &def, value)
    }

    /// Set an attribute validated against `def` — the road for custom
    /// definitions, and what [`set_attribute`](Self::set_attribute) calls.
    ///
    /// Refuses a personal-scope definition: personal values live in
    /// per-profile storage (D4), never on the shared node.
    pub fn set_attribute_defined(
        &mut self,
        node: NodeId,
        def: &AttributeDefinition,
        value: AttrValue,
    ) -> Result<AttrId> {
        check_settable(def, &value)?;
        self.check_due_target(node, &def.key)?;
        let existing = self.attrs_under(node, &def.key)?;
        match def.cardinality {
            Cardinality::One => {
                if let [only] = existing.as_slice() {
                    if only.value == value {
                        // Same value: no op, no phantom Ctrl+Z.
                        return Ok(only.id);
                    }
                }
                self.checkpoint();
                self.atomic(|ws| ws.set_one_attr_raw(node, def, existing, value))
            }
            Cardinality::Many => {
                if let Some(already) = existing.iter().find(|a| a.value == value) {
                    return Ok(already.id);
                }
                self.checkpoint();
                self.set_attr(node, &Attr::new(def.key.clone(), value))
            }
        }
    }

    /// Set a **system** attribute on several nodes as **one undo step**.
    ///
    /// The multi-select form of [`set_attribute`](Self::set_attribute): one
    /// checkpoint, one atomic group, so Ctrl+Z takes the whole gesture back.
    /// Nodes already carrying the value are skipped, and if every node does the
    /// whole call is a guarded no-op — answers whether anything changed.
    ///
    /// It deliberately does not loop over `set_attribute`: each of those opens
    /// and closes an undo group of its own, which would make an n-node apply n
    /// undo steps.
    ///
    /// **Group behavior, shared by every `*_on_all` helper here:** inside an
    /// open [`edit_group`](Self::edit_group) it joins the caller's group (so
    /// the editor can bundle the trigger-text deletion in); standalone,
    /// callers `checkpoint()` first so the apply does not fold into typing —
    /// the `trash` contract.
    pub fn set_attribute_on_all(
        &mut self,
        nodes: &[NodeId],
        key: &str,
        value: AttrValue,
    ) -> Result<bool> {
        let def =
            system_definition(key).ok_or_else(|| StoreError::UnknownAttribute(key.to_owned()))?;
        self.set_attribute_defined_on_all(nodes, &def, value)
    }

    /// [`set_attribute_on_all`](Self::set_attribute_on_all) against a
    /// definition the caller holds — the multi-select apply for a **custom**
    /// attribute, which would otherwise have to be written raw and unvalidated.
    ///
    /// The same undo contract: one checkpoint's worth of work in one atomic
    /// group, nodes already carrying the value skipped, and a guarded no-op
    /// answering `false` when every node does.
    pub fn set_attribute_defined_on_all(
        &mut self,
        nodes: &[NodeId],
        def: &AttributeDefinition,
        value: AttrValue,
    ) -> Result<bool> {
        check_settable(def, &value)?;
        // What each node needs, decided before anything is written.
        let mut work: Vec<(NodeId, Vec<Attr>)> = Vec::new();
        for &node in nodes {
            self.check_due_target(node, &def.key)?;
            let existing = self.attrs_under(node, &def.key)?;
            let already = match def.cardinality {
                Cardinality::One => {
                    matches!(existing.as_slice(), [only] if only.value == value)
                }
                Cardinality::Many => existing.iter().any(|a| a.value == value),
            };
            if !already {
                work.push((node, existing));
            }
        }
        if work.is_empty() {
            return Ok(false);
        }
        self.atomic(|ws| {
            for (node, existing) in work {
                match def.cardinality {
                    Cardinality::One => {
                        ws.set_one_attr_raw(node, def, existing, value.clone())?;
                    }
                    Cardinality::Many => {
                        ws.set_attr(node, &Attr::new(def.key.clone(), value.clone()))?;
                    }
                }
            }
            Ok(())
        })?;
        Ok(true)
    }

    /// The cardinality-one write, inside an already-open undo group. Folds
    /// convergence duplicates into the lowest id — the `ensure_trash_root`
    /// idiom at attr level.
    ///
    /// `pub(crate)` for [`crate::tasks`], whose commands validate in preflight
    /// and then write over records preflight already read: it is one of the
    /// four operations ADR-272's proof allows after the first write, and
    /// re-reading them would put a fallible lookup back into the mutation
    /// phase.
    pub(crate) fn set_one_attr_raw(
        &mut self,
        node: NodeId,
        def: &AttributeDefinition,
        existing: Vec<Attr>,
        value: AttrValue,
    ) -> Result<AttrId> {
        match existing.split_first() {
            // `read_attrs` sorts by (role, id), so `keep` is the
            // lowest id — the one every peer picks without talking.
            Some((keep, duplicates)) => {
                for dup in duplicates {
                    self.remove_attr(node, dup.id)?;
                }
                self.set_attr(
                    node,
                    &Attr {
                        id: keep.id,
                        role: def.key.clone(),
                        value,
                        label: None,
                    },
                )
            }
            None => self.set_attr(node, &Attr::new(def.key.clone(), value)),
        }
    }

    /// SPEC §4's exclusion, at a generic door rather than only in the UI: a
    /// kind that cannot carry task state cannot be given a due either, because
    /// a due **is** a task ([ADR-285]). Applies to the legacy bare role as well
    /// as [`KEY_DUE`], and to clearing as well as setting — one rule, so a
    /// contact cannot end up with a deadline by whichever door was not guarded.
    ///
    /// Cheap where it does not apply: nothing is read unless the key is a due.
    ///
    /// [ADR-285]: ../../../docs/decisions/ADR-276-300.md#adr-285-taskmodel--a-task-is-a-marked-node-and-the-marker-or-a-due-date-is-the-whole-derivation
    fn check_due_target(&self, node: NodeId, key: &str) -> Result<()> {
        if !is_due_key(key) {
            return Ok(());
        }
        if !crate::tasks::task_capable(self.read(node)?.kind) {
            return Err(StoreError::TaskRejected {
                node,
                why: crate::tasks::TaskRefusal::KindNotCapable,
            });
        }
        Ok(())
    }

    /// Every attr on `node` whose role is `key`.
    fn attrs_under(&self, node: NodeId, key: &str) -> Result<Vec<Attr>> {
        Ok(self
            .read(node)?
            .content
            .attrs()
            .iter()
            .filter(|a| a.role == key)
            .cloned()
            .collect())
    }

    /// Remove every value under `key`. Answers whether anything was there —
    /// and clearing an absent key is a **guarded no-op**: a delete of an
    /// absent key is still an op and therefore a phantom undo step
    /// (`set_heading`'s guard, applied to attributes).
    pub fn clear_attribute(&mut self, node: NodeId, key: &str) -> Result<bool> {
        // `clear_attribute` does not pass through `check_settable`, so the
        // command-only guard is repeated at its head — the alternative is a key
        // that cannot be written by this door but can be deleted by it
        // ([ADR-286]).
        //
        // [ADR-286]: ../../../docs/decisions/ADR-276-300.md#adr-286-taskboundary--six-canonical-commands-own-task-state-and-every-host-is-a-thin-wrapper-around-them
        if is_command_only_key(key) {
            return Err(command_only(key));
        }
        self.check_due_target(node, key)?;
        let doomed: Vec<AttrId> = self
            .read(node)?
            .content
            .attrs()
            .iter()
            .filter(|a| a.role == key)
            .map(|a| a.id)
            .collect();
        if doomed.is_empty() {
            return Ok(false);
        }
        self.checkpoint();
        self.atomic(|ws| {
            for id in doomed {
                ws.remove_attr(node, id)?;
            }
            Ok(())
        })?;
        Ok(true)
    }

    /// Tag a node. One attr per tag under [`KEY_TAG`], each value the
    /// [`TagId`] — which is what makes concurrent adds from two peers merge
    /// to the union. Adding a tag the node already carries is a no-op.
    ///
    /// The id's existence in the tag table is the caller's to have arranged
    /// (the table lives in the system document, which this document cannot
    /// see); an id the table has forgotten simply resolves to no name.
    pub fn add_tag(&mut self, node: NodeId, tag: TagId) -> Result<()> {
        let current = self.read(node)?;
        if current.tag_ids().any(|t| t == tag) {
            return Ok(());
        }
        self.checkpoint();
        self.set_attr(node, &Attr::new(KEY_TAG, AttrValue::Text(tag.to_storage())))?;
        Ok(())
    }

    /// Remove every value under `key` from several nodes as **one undo step**.
    /// Guarded like [`clear_attribute`](Self::clear_attribute): nothing there,
    /// no op. Answers whether anything went.
    pub fn clear_attribute_on_all(&mut self, nodes: &[NodeId], key: &str) -> Result<bool> {
        if is_command_only_key(key) {
            return Err(command_only(key));
        }
        let mut doomed: Vec<(NodeId, AttrId)> = Vec::new();
        for &node in nodes {
            self.check_due_target(node, key)?;
            for attr in self.attrs_under(node, key)? {
                doomed.push((node, attr.id));
            }
        }
        if doomed.is_empty() {
            return Ok(false);
        }
        self.atomic(|ws| {
            for (node, id) in doomed {
                ws.remove_attr(node, id)?;
            }
            Ok(())
        })?;
        Ok(true)
    }

    /// Tag several nodes as **one undo step** — the multi-select apply. Nodes
    /// already carrying the tag are skipped; if all do, a guarded no-op.
    /// Answers whether anything changed.
    pub fn add_tag_to_all(&mut self, nodes: &[NodeId], tag: TagId) -> Result<bool> {
        let mut missing: Vec<NodeId> = Vec::new();
        for &node in nodes {
            if !self.read(node)?.tag_ids().any(|t| t == tag) {
                missing.push(node);
            }
        }
        if missing.is_empty() {
            return Ok(false);
        }
        self.atomic(|ws| {
            for node in missing {
                ws.set_attr(node, &Attr::new(KEY_TAG, AttrValue::Text(tag.to_storage())))?;
            }
            Ok(())
        })?;
        Ok(true)
    }

    /// Untag several nodes as **one undo step**. Guarded; answers whether
    /// anything changed.
    pub fn remove_tag_from_all(&mut self, nodes: &[NodeId], tag: TagId) -> Result<bool> {
        let mut doomed: Vec<(NodeId, AttrId)> = Vec::new();
        for &node in nodes {
            for attr in self.attrs_under(node, KEY_TAG)? {
                if attr.as_text().and_then(TagId::parse) == Some(tag) {
                    doomed.push((node, attr.id));
                }
            }
        }
        if doomed.is_empty() {
            return Ok(false);
        }
        self.atomic(|ws| {
            for (node, id) in doomed {
                ws.remove_attr(node, id)?;
            }
            Ok(())
        })?;
        Ok(true)
    }

    /// Put a node in a list — one [`KEY_LIST`] value, replacing whatever list
    /// it was in. Setting the list it is already in is a guarded no-op, and a
    /// convergence duplicate folds into the lowest [`AttrId`], because this is
    /// [`set_attribute`](Self::set_attribute) rather than a second door.
    ///
    /// The list's existence in the workspace list table is the caller's to have
    /// arranged — this document cannot see that table — and an id the table has
    /// forgotten resolves to no list, exactly as a forgotten tag resolves to no
    /// name.
    ///
    /// [`ListId`]: crate::ids::ListId
    pub fn set_list(&mut self, node: NodeId, list: ListId) -> Result<AttrId> {
        self.set_attribute(node, KEY_LIST, AttrValue::Text(list.to_storage()))
    }

    /// Take a node out of whatever list it is in. Clearing an absent membership
    /// is a guarded no-op; answers whether anything went.
    pub fn clear_list(&mut self, node: NodeId) -> Result<bool> {
        self.clear_attribute(node, KEY_LIST)
    }

    /// **Clear one list's memberships across this document** — what a host runs
    /// after [`SystemDoc::delete_list`], on each document it has open.
    ///
    /// Deleting a list never deletes a task (SPEC §8): the registry record is
    /// tombstoned, every membership pointing at it stops resolving *there and
    /// then*, and this is the tidy-up that takes the dead value off the nodes.
    /// It is deliberately a **document-side sweep** rather than something the
    /// registry does, because the registry cannot see a node — the two live in
    /// different Loro documents, and a workspace's other documents may not even
    /// be open when the delete happens. A document swept later is not a
    /// different answer, only a later one; until then the stale value resolves
    /// to no list and the task shows in no bucket but Today's.
    ///
    /// The whole sweep is **one undo step**, and a document holding no such
    /// membership writes nothing at all. Walks the trash too, so a binned task
    /// does not come back out of the bin still carrying a list that is gone.
    ///
    /// [`SystemDoc::delete_list`]: crate::system::SystemDoc::delete_list
    pub fn clear_list_membership(&mut self, list: ListId) -> Result<usize> {
        let wanted = list.to_storage();
        let mut doomed: Vec<(NodeId, AttrId)> = Vec::new();
        for (node, _) in self.walk_with_trash()? {
            for attr in self.attrs_under(node, KEY_LIST)? {
                if attr.as_text().is_some_and(|t| t == wanted) {
                    doomed.push((node, attr.id));
                }
            }
        }
        if doomed.is_empty() {
            return Ok(0);
        }
        let cleared = doomed.len();
        self.checkpoint();
        self.atomic(|ws| {
            for (node, id) in doomed {
                ws.remove_attr(node, id)?;
            }
            Ok(())
        })?;
        Ok(cleared)
    }

    /// Untag a node. Removing a tag it does not carry is a guarded no-op.
    pub fn remove_tag(&mut self, node: NodeId, tag: TagId) -> Result<()> {
        let doomed: Vec<AttrId> = self
            .read(node)?
            .content
            .attrs()
            .iter()
            .filter(|a| a.role == KEY_TAG && a.as_text().and_then(TagId::parse) == Some(tag))
            .map(|a| a.id)
            .collect();
        if doomed.is_empty() {
            return Ok(());
        }
        self.checkpoint();
        self.atomic(|ws| {
            for id in doomed {
                ws.remove_attr(node, id)?;
            }
            Ok(())
        })?;
        Ok(())
    }
}

#[cfg(test)]
mod tests;
