// SPDX-License-Identifier: MPL-2.0

//! **Every event this workspace publishes, declared once.**
//!
//! The same contract as `koru-env`'s `VARS` and `koru-keys`' chord registry:
//! a table beside the code drifts, so the code goes through the table —
//! [`crate::event_name!`] resolves literals against it at compile time,
//! [`crate::validate`] refuses anything it does not declare at the boundary,
//! `reference/EVENTS.md` is generated from it and never hand-edited, and the tests
//! in this crate hold the grammar, the vocabularies and the privacy classes
//! to their rules.
//!
//! **Adding an event** means adding a row here (and usually a typed payload in
//! [`crate::catalog`]), then regenerating the doc:
//! `KORU_UPDATE_EVENTS=1 cargo test -p koru-events`. An attr that a telemetry
//! policy would export needs a privacy-class review first — see the
//! [`FieldClass`] docs.

/// Domain events are facts about the workspace and are kept longer;
/// instrumentation events are usage observations and are pruned early.
/// Both land in the one journal — the class is retention and intent, not a
/// second pipeline.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EventClass {
    Domain,
    Instrumentation,
}

impl EventClass {
    pub fn as_str(self) -> &'static str {
        match self {
            EventClass::Domain => "domain",
            EventClass::Instrumentation => "instrumentation",
        }
    }
}

/// Whether — and how — an event may ever leave the device. `Never` is the
/// default posture; anything else is a decision the registry row records.
/// Whatever the policy, identity and `LocalReference` attrs are stripped
/// mechanically by [`crate::telemetry::project`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TelemetryPolicy {
    /// Local bus only, forever.
    Never,
    /// Operational health signals — content-free, aggregate.
    Operational,
    /// Usage signals with no installation identity attached.
    AnonymousUsage,
    /// Usage signals that may join the consented, rotating installation id.
    PseudonymousUsage,
}

impl TelemetryPolicy {
    pub fn exportable(self) -> bool {
        !matches!(self, TelemetryPolicy::Never)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            TelemetryPolicy::Never => "never",
            TelemetryPolicy::Operational => "operational",
            TelemetryPolicy::AnonymousUsage => "anonymous-usage",
            TelemetryPolicy::PseudonymousUsage => "pseudonymous-usage",
        }
    }
}

/// The declarable privacy classes. Note what is **not** here: "forbidden
/// content" and "secret identity" (PRODUCT_METRICS.md's other two rows) are
/// not variants — they cannot be declared, which is the point. They exist
/// only as rejection vocabulary in validation errors and the ingest contract.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FieldClass {
    /// Closed vocabulary, cardinality-reviewed. Exportable.
    ExactCategorical,
    /// Bucketed client-side. Exportable.
    BucketedNumeric,
    /// An optional user-answered question. Exportable.
    ExplicitSurvey,
    /// A store id or other reference: valid on the local bus (events carry
    /// references, not content), NEVER exported.
    LocalReference,
}

impl FieldClass {
    pub fn exportable(self) -> bool {
        !matches!(self, FieldClass::LocalReference)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            FieldClass::ExactCategorical => "exact-categorical",
            FieldClass::BucketedNumeric => "bucketed-numeric",
            FieldClass::ExplicitSurvey => "explicit-survey",
            FieldClass::LocalReference => "local-reference",
        }
    }
}

/// Which [`crate::AttrValue`] variant a key takes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AttrKind {
    Id,
    Time,
    Token,
    Count,
    Int,
    Version,
    Bucket,
    Flag,
}

impl AttrKind {
    pub fn as_str(self) -> &'static str {
        match self {
            AttrKind::Id => "id",
            AttrKind::Time => "time",
            AttrKind::Token => "token",
            AttrKind::Count => "count",
            AttrKind::Int => "int",
            AttrKind::Version => "version",
            AttrKind::Bucket => "bucket",
            AttrKind::Flag => "flag",
        }
    }
}

/// One declared attribute on one event.
pub struct AttrSpec {
    pub key: &'static str,
    pub kind: AttrKind,
    pub class: FieldClass,
    pub required: bool,
    /// For `Token` attrs: the closed vocabulary. Empty for every other kind.
    pub vocabulary: &'static [&'static str],
    /// Developer prose for reference/EVENTS.md. Not translated: nothing here is
    /// user-facing.
    pub doc: &'static str,
}

/// One event.
pub struct EventSpec {
    pub name: &'static str,
    pub version: u16,
    pub class: EventClass,
    pub telemetry: TelemetryPolicy,
    pub attrs: &'static [AttrSpec],
    /// Developer prose. Where an emitter carries a load-bearing semantic —
    /// coalescing, throttling — this is where it is pinned, so the emitter
    /// cannot quietly reinterpret it.
    pub doc: &'static str,
}

impl EventSpec {
    pub fn attr(&self, key: &str) -> Option<&'static AttrSpec> {
        self.attrs.iter().find(|attr| attr.key == key)
    }
}

const fn token_attr(
    key: &'static str,
    vocabulary: &'static [&'static str],
    doc: &'static str,
) -> AttrSpec {
    AttrSpec {
        key,
        kind: AttrKind::Token,
        class: FieldClass::ExactCategorical,
        required: true,
        vocabulary,
        doc,
    }
}

const fn to_build_attr() -> AttrSpec {
    count_attr(
        "to_build",
        "The build number the update leads to. A count, not a bucket: build \
         numbers are the one numeric whose exact value is the point, and \
         their cardinality is the release history's.",
    )
}

/// The supported languages plus `other` — the mapping PRODUCT_METRICS asks
/// for: an unknown value becomes `other` on this machine, so a raw locale
/// tag never leaves it.
pub const LANGUAGE_TOKENS: &[&str] = &[
    "en", "es", "pt_br", "de", "fr", "ar", "zh_hans", "ja", "other",
];

/// The same spec, not required — what a platform that did not answer omits.
const fn optional(attr: AttrSpec) -> AttrSpec {
    AttrSpec {
        required: false,
        ..attr
    }
}

const fn count_attr(key: &'static str, doc: &'static str) -> AttrSpec {
    AttrSpec {
        key,
        kind: AttrKind::Count,
        // Exact rather than bucketed, and the class says so: these are
        // low-cardinality machine facts (a resolution, a core count), each
        // reviewed where it is declared.
        class: FieldClass::ExactCategorical,
        required: true,
        vocabulary: &[],
        doc,
    }
}

const fn flag_attr(key: &'static str, doc: &'static str) -> AttrSpec {
    AttrSpec {
        key,
        kind: AttrKind::Flag,
        class: FieldClass::ExactCategorical,
        required: true,
        vocabulary: &[],
        doc,
    }
}

/// A reference to a record this device holds — the [`FieldClass::LocalReference`]
/// kind, stripped mechanically before anything is exported. Used where the
/// identity the event is about has no home in the envelope: an envelope names
/// a workspace (`scope.workspace_id`, `subject`) but its `scope.profile_id` is
/// *the emitting process's* profile, which is not the same question as "which
/// profile record changed".
const fn reference_attr(key: &'static str, doc: &'static str) -> AttrSpec {
    AttrSpec {
        key,
        kind: AttrKind::Id,
        class: FieldClass::LocalReference,
        required: true,
        vocabulary: &[],
        doc,
    }
}

const fn bucket_attr(key: &'static str, doc: &'static str) -> AttrSpec {
    AttrSpec {
        key,
        kind: AttrKind::Bucket,
        class: FieldClass::BucketedNumeric,
        required: true,
        vocabulary: &[],
        doc,
    }
}

/// A reference to something in the model — a node, a document. Always
/// `LocalReference`: an id is valid on the local bus and is stripped
/// mechanically at any telemetry boundary, whatever the event's policy says.
const fn id_attr(key: &'static str, doc: &'static str) -> AttrSpec {
    AttrSpec {
        key,
        kind: AttrKind::Id,
        class: FieldClass::LocalReference,
        required: true,
        vocabulary: &[],
        doc,
    }
}

/// An exact wall-clock instant used as a local fact identity, never as an
/// exportable measurement. Exact times can identify a person's activity, so
/// they remain [`FieldClass::LocalReference`] even on a `Never` row: a later
/// policy change cannot accidentally make them leave the device.
const fn time_attr(key: &'static str, doc: &'static str) -> AttrSpec {
    AttrSpec {
        key,
        kind: AttrKind::Time,
        class: FieldClass::LocalReference,
        required: true,
        vocabulary: &[],
        doc,
    }
}

/// A signed quantity — a delta, a duration in milliseconds. `count_attr`
/// for anything that cannot be negative.
const fn int_attr(key: &'static str, doc: &'static str) -> AttrSpec {
    AttrSpec {
        key,
        kind: AttrKind::Int,
        class: FieldClass::ExactCategorical,
        required: true,
        vocabulary: &[],
        doc,
    }
}

// ------------------------------------------------ the delegated-run vocabularies
//
// Each closed set below mirrors an enum in `koru-delegate-core` (or, for the
// agent and the scope overlap, a closed set the builder crate owns), and
// `koru-delegate-events` maps every variant to its token by exhaustive
// `match` — so a variant added to the enum is a build error at the builder
// until the vocabulary here grows with it, never a token outside the row.

/// `delegate.run.created`'s `agent` — the provider category the run was
/// delegated to. The four P1.1 evaluated, plus `other`: an unrecognised
/// `ProviderKind` collapses to `other` **on this machine** (the
/// `LANGUAGE_TOKENS` rule), so a provider token never leaves it.
pub const DELEGATE_AGENT_TOKENS: &[&str] = &["codex", "claude", "gemini", "opencode", "other"];

/// `WorkspaceScope`'s variants. `none` is a run created before a scope was
/// chosen; P1.5 refuses to *start* it, so it can be created and never run.
const DELEGATE_SCOPE_KINDS: &[&str] = &["none", "project_root", "project_folder", "temporary"];

/// `PriorAttempt` (§7.6): what recovery could prove about the attempt a
/// restart interrupted.
const DELEGATE_PRIOR_ATTEMPT: &[&str] = &["ended", "unknown"];

/// `ResumeSeam` (P1.9): the adapter's own resume token, or the objective
/// re-submitted with the recorded pins and the artifacts so far.
const DELEGATE_RESUME_SEAM: &[&str] = &["native", "recorded_context"];

/// D29: a completed run that registered no artifact is finished needing
/// attention, never a delivered result.
const DELEGATE_COMPLETION: &[&str] = &["delivered", "needs_attention"];

/// `FailureStage`: where a start or a turn failed. A stage, never a message.
const DELEGATE_FAILURE_STAGE: &[&str] = &[
    "scope_admission",
    "context_capture",
    "context_materialization",
    "session_creation",
    "provider_start",
    "authority",
    "turn",
];

/// `CancelReason` (D36). Only `run` terminalizes; `takeover_boundary` ends
/// the step so a person can take control, and the run stays running.
const DELEGATE_CANCEL_REASON: &[&str] = &["run", "takeover_boundary"];

/// `TurnOriginator` (§7.5): the orchestrator pursuing the objective, or a
/// person who holds control.
const DELEGATE_ORIGINATOR: &[&str] = &["orchestrator", "human"];

/// How a turn's coalesced tool calls went: every call succeeded, some
/// failed, or every one did.
const DELEGATE_TOOL_OUTCOME: &[&str] = &["succeeded", "partial", "failed"];

/// `ApprovalOutcome`.
const DELEGATE_APPROVAL_OUTCOME: &[&str] = &["approved", "rejected", "expired"];

/// Why an attachment was refused — the session protocol's two attachment
/// error codes, and `other` for any other refusal. Never the code's text.
const DELEGATE_ATTACHMENT_REJECTION: &[&str] = &["not_found", "limit", "other"];

/// `CachedInputRelationship` (§17.2, wire v14): whether `cached_input` is
/// inside `input` or beside it, or the adapter has not said.
const DELEGATE_CACHED_RELATIONSHIP: &[&str] = &["included", "separate", "undeclared"];

/// `Completeness` folded with its `IncompleteReason` into one token, so a
/// usage row says in one word whether a price over it would be a guess and
/// why.
const DELEGATE_COMPLETENESS: &[&str] = &[
    "complete",
    "cached_relationship_undeclared",
    "multiple_models",
    "no_usage_reported",
];

/// The terminal `RunStatus` a run's usage fold was published under.
const DELEGATE_RUN_OUTCOME: &[&str] = &["completed", "failed", "cancelled"];

/// `Rating` (D44): one gesture. The note is never here.
const DELEGATE_RATING: &[&str] = &["good", "not_good"];

/// How the scope asking to start relates to the one that holds it
/// (`koru-delegate-scope`'s `Overlap`, from the candidate's side): the same
/// directory, inside the holder's, or around it. `disjoint` is not a
/// vocabulary entry because a disjoint scope is not busy.
const DELEGATE_SCOPE_OVERLAP: &[&str] = &["same", "inside", "around"];

/// Why an active schedule became paused. The context detail is split into
/// content-free item ordinal and stage attributes, never a document identity.
const DELEGATE_SCHEDULE_PAUSE_REASON: &[&str] = &[
    "manual",
    "provider_not_delegatable",
    "scope_unavailable",
    "context_unavailable",
];

/// Where resolving one schedule context pointer failed.
const DELEGATE_SCHEDULE_CONTEXT_STAGE: &[&str] = &["resolve", "commit", "retention", "materialize"];

/// D25 and D34's only two settled no-run outcomes.
const DELEGATE_SCHEDULE_SKIP_REASON: &[&str] = &["missed", "busy"];

/// `task.created`'s `reason` — why the task exists (SPEC §3's marker value).
const TASK_CREATED_REASON: &[&str] = &["explicit", "due", "tasks_view"];

/// The kinds a node may be when it becomes a task — SPEC §4's closed set.
const TASK_NODE_KINDS: &[&str] = &["paragraph", "task", "document", "image", "file"];

/// What a Due or Priority change did to the value. `set` is first-time,
/// `changed` is one value for another, `cleared` is removal — three words
/// rather than a before/after pair, because the values themselves never
/// appear in an event.
const TASK_ATTRIBUTE_CHANGE: &[&str] = &["set", "changed", "cleared"];

/// The first segment of every name comes from this list, so the namespace
/// grows by review rather than by typo. `agent`, `notification` and `flow`
/// are reserved for Milestone 3; nothing registers under them yet.
///
/// `profile` was added for the Store's catalog facts: a profile is a
/// first-class catalog record with its own routes, and folding its lifecycle
/// under `workspace` would say the wrong thing about what changed.
pub const AREAS: &[&str] = &[
    "app",
    "account",
    "profile",
    "workspace",
    "document",
    "search",
    "import",
    "sync",
    "voice",
    "agent",
    "notification",
    "flow",
    "terminal",
    "task",
    "reminder",
    "due",
    "entity",
    // The settings authority's change facts. A first-class area rather than a
    // corner of `app`: the authority is one process-wide owner with its own
    // routes, and folding its facts under `app` would say the wrong thing
    // about what changed and who to ask about it.
    "settings",
    // Delegated runs (docs/architecture/eventbus/README.md). Its own
    // area rather than a corner of `agent` or `terminal`: a run is a
    // manifest-registered document with a lifecycle of its own, and the
    // only crate that spells these names is `koru-delegate-events`.
    "delegate",
];

/// Which speech runtime ran — `voice.dictation.completed`'s and
/// `voice.model.installed`'s `engine` vocabulary.
///
/// Both product runtimes ship on desktop (ADR-253-voiceruntimes), so both are
/// declared and the vocabulary is closed today. `other` is the same
/// local-mapping rule [`LANGUAGE_TOKENS`] uses: a `koru_voice::EngineId` the
/// emitter does not recognise becomes `other` **on this machine**, so a raw
/// engine string can never leave it.
///
/// What changes when the engine gate's measurement (Plan 0.4) runs: if a
/// runtime is dropped, its token is removed and the version is bumped; if a
/// third runtime is adopted, its token is added and the version is bumped.
/// Until either happens a third engine reports as `other` — degraded, never
/// leaky, and never a build error at the emitter.
pub const VOICE_ENGINE_TOKENS: &[&str] = &["sherpa", "whisper", "other"];

/// The two quality tiers `koru_voice_models::ModelTier` declares, which is the
/// closed set a person actually chooses between. The **model id** is
/// deliberately not an attr: `parakeet-tdt-v3` is a name from a remote,
/// versioned catalog that anticipates third-party entries, so no closed
/// vocabulary for it exists and declaring one would be freezing a list that
/// lives on a server.
pub const VOICE_TIER_TOKENS: &[&str] = &["compact", "high_accuracy"];

/// The table. Domain lifecycle first, then app-side instrumentation.
pub const REGISTRY: &[EventSpec] = &[
    EventSpec {
        name: "reminder.rule.changed",
        version: 1,
        class: EventClass::Domain,
        telemetry: TelemetryPolicy::Never,
        attrs: &[],
        doc: "A synchronized reminder rule or its monotonic deletion fact changed. Content-free and profile-scoped; consumers re-read the authoritative reconciliation snapshot.",
    },
    EventSpec {
        name: "reminder.action.appended",
        version: 1,
        class: EventClass::Domain,
        telemetry: TelemetryPolicy::Never,
        attrs: &[],
        doc: "A synchronized occurrence action was appended. Content-free and profile-scoped; the action and reminder are read from authoritative state.",
    },
    EventSpec {
        name: "reminder.history.cleared",
        version: 1,
        class: EventClass::Domain,
        telemetry: TelemetryPolicy::Never,
        attrs: &[],
        doc: "Handled reminder history was explicitly cleared. Content-free and profile-scoped; active and snoozed actions were untouched.",
    },
    EventSpec {
        name: "reminder.settings.changed",
        version: 1,
        class: EventClass::Domain,
        telemetry: TelemetryPolicy::Never,
        attrs: &[],
        doc: "End-of-day or default-tone reminder settings changed. Content-free and profile-scoped.",
    },
    EventSpec {
        name: "profile.dictionary.changed",
        version: 1,
        class: EventClass::Domain,
        telemetry: TelemetryPolicy::Never,
        attrs: &[],
        doc: "The synchronized personal spelling dictionary changed. Content-free and profile-scoped; consumers re-read the authoritative private Profile unit.",
    },
    EventSpec {
        name: "due.changed",
        version: 1,
        class: EventClass::Domain,
        telemetry: TelemetryPolicy::Never,
        attrs: &[AttrSpec {
            key: "entity_id",
            kind: AttrKind::Id,
            class: FieldClass::LocalReference,
            required: true,
            vocabulary: &[],
            doc: "The changed node id, meaningful with the document subject.",
        }],
        doc: "A projected koru.due value was set, moved, or cleared. Derived from the unthrottled semantic projection diff; ids only, and consumers re-read current state.",
    },
    EventSpec {
        name: "entity.removed",
        version: 1,
        class: EventClass::Domain,
        telemetry: TelemetryPolicy::Never,
        attrs: &[AttrSpec {
            key: "entity_id",
            kind: AttrKind::Id,
            class: FieldClass::LocalReference,
            required: true,
            vocabulary: &[],
            doc: "The removed node id, meaningful with the document subject.",
        }],
        doc: "A previously projected entity disappeared from a current document projection. This observed transition is a cleanup wake, never permission to infer deletion from a cold projection.",
    },
    EventSpec {
        name: "document.created",
        version: 1,
        class: EventClass::Domain,
        telemetry: TelemetryPolicy::AnonymousUsage,
        attrs: &[],
        doc: "A document exists that did not before. Derived by the events \
              worker from the store's commit feed (first sight of the \
              document in doc_state); subject = the document. On a fresh \
              journal the baseline seeding pass suppresses this for \
              documents that already existed.",
    },
    EventSpec {
        name: "document.updated",
        version: 1,
        class: EventClass::Domain,
        telemetry: TelemetryPolicy::AnonymousUsage,
        attrs: &[],
        doc: "A known document's content changed. Derived from the commit \
              feed and THROTTLED: at most one per document per 15 minutes, \
              so it means 'was edited around then', never 'one edit'. \
              Consumers wanting every commit belong on the store's own feed.",
    },
    EventSpec {
        name: "document.removed",
        version: 1,
        class: EventClass::Domain,
        telemetry: TelemetryPolicy::Never,
        attrs: &[],
        doc: "A document was dropped for good — not binned. Trash is a move \
              (document.trashed); this is the end of the id.",
    },
    EventSpec {
        name: "document.trashed",
        version: 1,
        class: EventClass::Domain,
        telemetry: TelemetryPolicy::Never,
        attrs: &[],
        doc: "A document was moved to the trash. It still resolves and can \
              be restored; subject = the document.",
    },
    EventSpec {
        name: "document.restored",
        version: 1,
        class: EventClass::Domain,
        telemetry: TelemetryPolicy::Never,
        attrs: &[],
        doc: "A document came back out of the trash.",
    },
    EventSpec {
        name: "workspace.created",
        version: 1,
        class: EventClass::Domain,
        telemetry: TelemetryPolicy::AnonymousUsage,
        attrs: &[token_attr(
            "source",
            &["onboarding", "manual", "import"],
            "How the workspace came to be.",
        )],
        doc: "The app created a workspace. Emitted at the creation call \
              sites (the app knows the source; the store layer cannot tell \
              creation from first sight); subject = the workspace.",
    },
    EventSpec {
        name: "workspace.observed",
        version: 1,
        class: EventClass::Domain,
        telemetry: TelemetryPolicy::AnonymousUsage,
        attrs: &[],
        doc: "A workspace became visible on this device that the journal had \
              never seen — derived by the events worker from the commit \
              feed, so it covers every writer: this app, a phone, an MCP \
              tool, a direct API client, or a peer's workspace arriving by \
              sync. Deliberately NOT `workspace.created`: the store layer \
              cannot tell creation from first sight, and a workspace synced \
              from another device was not created here. `workspace.created` \
              remains the app's own precise signal, carrying the `source` \
              only it knows; the two answer different questions and are not \
              a double count of one.",
    },
    EventSpec {
        name: "workspace.deleted",
        version: 1,
        class: EventClass::Domain,
        telemetry: TelemetryPolicy::AnonymousUsage,
        attrs: &[],
        doc: "The app deleted a workspace — a soft delete the store purges \
              after its retention window, so restoring one is possible and \
              does not un-say this. Emitted at the delete call sites; \
              subject = the workspace.",
    },
    // ------------------------------------------------------------- task activity
    //
    // **The twelve, from ADR-273, implemented verbatim.** Every one of them is
    // `Domain` (SPEC §9's 90-day retention — task activity is a work log, not
    // an observation about the app) and `TelemetryPolicy::Never`. Never rather
    // than anonymous-usage deliberately: an exportable policy needs a
    // privacy-class review (ADR-166) that this milestone is not the place for.
    // Reversing that is a review, not an edit.
    //
    // Every row's subject is `SubjectKind::Task`, whose id carries the
    // `task:<workspace>:<document>:<node>` spelling (SPEC §3), and every one
    // sets `scope.workspace_id`. No date, no title, no text, no tag name, no
    // color and no custom value appears anywhere — and `AttrValue` has no
    // free-string variant, so that is structural rather than a promise.
    //
    // Tags, color and custom-attribute changes emit nothing this release.
    EventSpec {
        name: "task.created",
        version: 1,
        class: EventClass::Domain,
        telemetry: TelemetryPolicy::Never,
        attrs: &[
            token_attr(
                "reason",
                TASK_CREATED_REASON,
                "Why the task exists: asked for outright, implied by a due \
                 date, or created from the Tasks view.",
            ),
            token_attr(
                "node_kind",
                TASK_NODE_KINDS,
                "What the node was when it became a task — SPEC §4's closed \
                 task-capable set.",
            ),
        ],
        doc: "A node became a task. Emitted by the `ensure_task` and `set_due` \
              canonical commands — `set_due` on a plain row both creates and \
              dates it, so one gesture is a `created` and a `due.changed` \
              sharing one correlation.",
    },
    EventSpec {
        name: "task.removed",
        version: 1,
        class: EventClass::Domain,
        telemetry: TelemetryPolicy::Never,
        attrs: &[],
        doc: "A node stopped being a task — Remove from Tasks. The node and \
              its content stay; only the task state goes. Emitted by \
              `remove_from_tasks`.",
    },
    EventSpec {
        name: "task.completed",
        version: 1,
        class: EventClass::Domain,
        telemetry: TelemetryPolicy::Never,
        attrs: &[token_attr(
            "reason",
            &[
                "explicit_complete",
                "checkbox_completed",
                "cascade_from_ancestor",
            ],
            "Which lifecycle reason completed it — `koru_core::LifecycleReason` \
             is the single source these tokens are spelled from.",
        )],
        doc: "A task was completed. Emitted by `complete_task` ONCE PER TASK, \
              including each task the cascade completed below it — a cascade \
              is a set of events sharing one correlation, not one event with a \
              count.",
    },
    EventSpec {
        name: "task.reopened",
        version: 1,
        class: EventClass::Domain,
        telemetry: TelemetryPolicy::Never,
        attrs: &[token_attr(
            "reason",
            &[
                "explicit_reopen",
                "checkbox_reopened",
                "unfinished_child_added",
            ],
            "Which lifecycle reason reopened it. `unfinished_child_added` is \
             SPEC §5's auto-reopen, spelled `UNFINISHED_CHILD_ADDED` there and \
             lowercase everywhere it is stored.",
        )],
        doc: "A completed task is open again. Emitted by `reopen_task` and by \
              the classification pass — one per task, so a reopen chain up the \
              ancestors is several events under one correlation.",
    },
    EventSpec {
        name: "task.due.changed",
        version: 1,
        class: EventClass::Domain,
        telemetry: TelemetryPolicy::Never,
        attrs: &[
            token_attr(
                "change",
                TASK_ATTRIBUTE_CHANGE,
                "Whether a deadline was set, moved, or removed.",
            ),
            token_attr(
                "temporal_kind",
                &["date", "datetime_zoned", "none"],
                "The SHAPE the deadline now has — a whole day, an instant in a \
                 zone, or nothing. Never the value: a reader who wants the \
                 date reads the task.",
            ),
        ],
        doc: "A task's Due changed. Emitted by `set_due` and `clear_due`. It \
              says THAT a deadline moved and what shape it now has, and \
              deliberately not what it is — no date, in any spelling, ever \
              enters an event.",
    },
    EventSpec {
        name: "task.priority.changed",
        version: 1,
        class: EventClass::Domain,
        telemetry: TelemetryPolicy::Never,
        attrs: &[
            token_attr(
                "change",
                TASK_ATTRIBUTE_CHANGE,
                "Whether a priority was set, moved, or removed.",
            ),
            optional(token_attr(
                "priority",
                &["low", "medium", "high", "urgent"],
                "The priority now carried. Absent when the change cleared it — \
                 absence means removed, never `low`.",
            )),
        ],
        doc: "A task's Priority changed. Emitted by the priority door and ONLY \
              when the target already `is_task` — setting a priority on a \
              plain row is not task activity and creates no task.",
    },
    EventSpec {
        name: "task.child.added",
        version: 1,
        class: EventClass::Domain,
        telemetry: TelemetryPolicy::Never,
        attrs: &[
            id_attr("child", "The node that arrived under this task."),
            token_attr(
                "child_class",
                &["unfinished_structural", "finished_structural", "passive"],
                "What the classification pass made of it — only an unfinished \
                 STRUCTURAL child can reopen an ancestor; passive content \
                 (a note, an image) never does.",
            ),
            flag_attr(
                "reopened",
                "Whether this arrival reopened the task. The companion \
                 `task.reopened` carries the reason; this is the same fact \
                 from the parent's side.",
            ),
        ],
        doc: "A child was inserted under a task. Emitted by the classification \
              pass from an insert. A faithful import emits nothing — imports \
              are restore, not editing, and run through the named exemption.",
    },
    EventSpec {
        name: "task.child.moved",
        version: 1,
        class: EventClass::Domain,
        telemetry: TelemetryPolicy::Never,
        attrs: &[
            id_attr("child", "The node that moved under this task."),
            token_attr(
                "child_class",
                &["unfinished_structural", "finished_structural", "passive"],
                "What the classification pass made of it, exactly as for an \
                 insert.",
            ),
            flag_attr("reopened", "Whether this move reopened the task."),
        ],
        doc: "A child moved under a task. Emitted by the classification pass \
              from a move — the same three attrs as an insert, because from \
              the task's side the two are the same news arriving differently.",
    },
    EventSpec {
        name: "task.child.removed",
        version: 1,
        class: EventClass::Domain,
        telemetry: TelemetryPolicy::Never,
        attrs: &[id_attr(
            "child",
            "The node that is no longer under this task.",
        )],
        doc: "A child left a task — deleted, or moved away. Emitted by the \
              structural delete/move-away hook. It carries no class: what the \
              child was is what the `added` event already said, and a departure \
              never reopens anything.",
    },
    EventSpec {
        name: "task.child.completed",
        version: 1,
        class: EventClass::Domain,
        telemetry: TelemetryPolicy::Never,
        attrs: &[
            id_attr("child", "The node that was completed."),
            flag_attr(
                "child_is_task",
                "Whether the completed child is itself a task or a plain \
                 checklist item — the parent's view of the difference.",
            ),
        ],
        doc: "Something under a task was completed. Emitted by `complete_task` \
              on a node whose NEAREST task ancestor exists — one event for that \
              ancestor, never one per ancestor up the tree.",
    },
    EventSpec {
        name: "task.child.reopened",
        version: 1,
        class: EventClass::Domain,
        telemetry: TelemetryPolicy::Never,
        attrs: &[
            id_attr("child", "The node that was reopened."),
            flag_attr(
                "child_is_task",
                "Whether the reopened child is itself a task or a plain \
                 checklist item.",
            ),
            flag_attr(
                "reopened_parent",
                "Whether this reopen also reopened the task above it — the \
                 cascade, from the parent's side.",
            ),
        ],
        doc: "Something under a task was reopened. Emitted by `reopen_task` \
              and by the checkbox-driven reopen, for the nearest task ancestor.",
    },
    EventSpec {
        name: "task.content.changed",
        version: 1,
        class: EventClass::Domain,
        telemetry: TelemetryPolicy::Never,
        attrs: &[],
        doc: "A task's own text was edited. COALESCED, and the boundary is part \
              of the contract: one event per task per editing BURST. Four \
              constraints, none of them negotiable by an emitter — NEVER PER \
              KEYSTROKE (the burst set is a map the edit path inserts into; \
              emission reads it); NO TIMER OF ITS OWN (it rides the host's \
              existing throttled maintenance tick — nothing in this feature \
              may put a new clock on the idle path); NO DOCUMENT WALK (the set \
              names the nodes that changed; nothing scans for them); and NEVER \
              THE TEXT, which is why this row has no attrs at all. The desktop \
              host wrapper owns the coalescing and a burst on a task ends at \
              the first of: the caret leaving its row, the maintenance tick \
              seeing five quiet seconds, or its tab closing. The MCP wrapper's \
              burst is THE CALL — one per `edit_document` that changed a task's \
              own text. It belongs to no command, so it carries neither \
              correlation nor causation: it is its own root.",
    },
    // ------------------------------------------------- delegated runs (P1.8)
    //
    // The facts of a delegated run, as `docs/architecture/eventbus/README.md` documents
    // them. Every row is `domain` and **`Never`** (D41): a run's history
    // syncs between a person's own devices and goes nowhere else, and the
    // economics rows in particular are the raw material of a cross-user
    // benchmark that is deliberately *not* smuggled in here — it needs its own
    // privacy-class review (ADR-166) and a consent flow.
    //
    // **Subject and correlation.** A run is a manifest-registered document
    // (`koru-run`, D14), so every row's subject is `SubjectKind::Document`
    // with the Run document's id — no new subject kind — and the `RunId`
    // travels as a typed `Id` attribute on every row instead. The chain root
    // is the real `EventId` of `delegate.run.created`; every later row carries
    // it as `correlation_id`, so one run's history is one `WHERE`.
    //
    // **What is never here.** No objective, no prompt, no tool argument, no
    // artifact title, no folder, no provider payload, and never the rating's
    // note (D44). Ids, counts, a signed elapsed time, flags and tokens from
    // the closed sets above. The model a turn used is an `Id` reference to
    // metadata retained in the Run document's `models` map (§17.3), which is
    // `LocalReference` and stripped mechanically — never a name.
    //
    // **Retry identity** (§7.1, §17.2). A re-publication from `runs.db`'s
    // pending list reuses the same `event_id`, which the journal dedupes; a
    // usage *amendment* is a new fact with a higher `revision` and a fresh
    // `event_id`, so deduplication cannot swallow a correction.
    //
    // The only crate that spells these names is `koru-delegate-events`: one
    // builder per row, taking typed ids and closed enums and
    // mapping each variant to its token by exhaustive `match`.
    EventSpec {
        name: "delegate.run.created",
        version: 1,
        class: EventClass::Domain,
        telemetry: TelemetryPolicy::Never,
        attrs: &[
            id_attr("run", "The run's own id, minted by the asking client."),
            id_attr(
                "workspace",
                "The workspace the run was delegated from — the same id the \
                 envelope's scope carries, repeated here because an activity \
                 row drops the scope.",
            ),
            token_attr(
                "agent",
                DELEGATE_AGENT_TOKENS,
                "The provider category delegated to. Never the launch \
                 profile's name.",
            ),
            token_attr(
                "scope_kind",
                DELEGATE_SCOPE_KINDS,
                "Which kind of working folder the run has. Never the folder.",
            ),
            count_attr(
                "context_items",
                "How many context items were pinned (§7.2). Never which.",
            ),
        ],
        doc: "The Run document was committed (§7.1 step 3) — the moment a run \
              exists. **The chain root**: its `event_id` is every later row's \
              `correlation_id`. The actor is whoever asked: a person, or the \
              orchestrator for a scheduled firing.",
    },
    EventSpec {
        name: "delegate.run.started",
        version: 1,
        class: EventClass::Domain,
        telemetry: TelemetryPolicy::Never,
        attrs: &[
            id_attr("run", "The run."),
            id_attr("attempt", "The attempt whose provider came up."),
            count_attr(
                "generation",
                "The attempt's generation, from 1. A resume mints the next.",
            ),
            id_attr("session", "The provider session the attempt runs in."),
        ],
        doc: "An attempt's provider is up (`Starting` → `Running`). One per \
              attempt: a resume is a new attempt and a new row.",
    },
    EventSpec {
        name: "delegate.run.interrupted",
        version: 1,
        class: EventClass::Domain,
        telemetry: TelemetryPolicy::Never,
        attrs: &[
            id_attr("run", "The run."),
            id_attr("attempt", "The attempt the restart ended."),
            count_attr("generation", "That attempt's generation."),
            token_attr(
                "prior_attempt",
                DELEGATE_PRIOR_ATTEMPT,
                "What recovery could prove (§7.6): the attempt ended, or \
                 nothing is known and a person decides.",
            ),
        ],
        doc: "A restart ended the attempt (§7.6). Recorded by the recovering \
              orchestrator, so it is timestamped at recovery, not at the crash.",
    },
    EventSpec {
        name: "delegate.run.resumed",
        version: 1,
        class: EventClass::Domain,
        telemetry: TelemetryPolicy::Never,
        attrs: &[
            id_attr("run", "The run."),
            id_attr("attempt", "The fresh attempt."),
            count_attr("generation", "The fresh attempt's generation."),
            token_attr(
                "via",
                DELEGATE_RESUME_SEAM,
                "Which seam the resume went through (P1.9: the timeline says \
                 which).",
            ),
        ],
        doc: "A fresh attempt continued an interrupted run — through the \
              adapter's own resume token, or by re-submitting the objective \
              with the recorded pins and the artifacts so far.",
    },
    EventSpec {
        name: "delegate.run.completed",
        version: 1,
        class: EventClass::Domain,
        telemetry: TelemetryPolicy::Never,
        attrs: &[
            id_attr("run", "The run."),
            id_attr(
                "attempt",
                "The attempt whose orchestrator turn completed it.",
            ),
            count_attr("generation", "That attempt's generation."),
            token_attr(
                "outcome",
                DELEGATE_COMPLETION,
                "Delivered a result, or finished needing attention because no \
                 artifact was registered (D29).",
            ),
            count_attr(
                "artifacts",
                "How many artifact versions the run registered in all.",
            ),
        ],
        doc: "The run reached `Completed`: an orchestrator-originated turn \
              completed and nothing was pending — no approval, no waiting \
              input, no fenced human control (§7.5). Terminal; never revised by \
              a late event from an older attempt.",
    },
    EventSpec {
        name: "delegate.run.failed",
        version: 1,
        class: EventClass::Domain,
        telemetry: TelemetryPolicy::Never,
        attrs: &[
            id_attr("run", "The run."),
            optional(id_attr(
                "attempt",
                "The attempt that failed. Absent when the failure came before \
                 any attempt existed (scope admission, context capture).",
            )),
            count_attr(
                "generation",
                "The current generation — 1 when no attempt had started.",
            ),
            token_attr(
                "stage",
                DELEGATE_FAILURE_STAGE,
                "Where it failed. A stage, never a message.",
            ),
        ],
        doc: "The run reached `Failed`. Only an orchestrator-originated turn \
              can fail a run; a human turn's failure is a `delegate.turn.failed` \
              and never this (§7.5). Terminal.",
    },
    EventSpec {
        name: "delegate.run.cancelled",
        version: 1,
        class: EventClass::Domain,
        telemetry: TelemetryPolicy::Never,
        attrs: &[
            id_attr("run", "The run."),
            optional(id_attr(
                "attempt",
                "The attempt cancelled. Absent for a run cancelled while \
                 `Created`.",
            )),
            count_attr(
                "generation",
                "The current generation — 1 when no attempt had started.",
            ),
            token_attr(
                "reason",
                DELEGATE_CANCEL_REASON,
                "`run`: the person ended the job and the run is `Cancelled`. \
                 `takeover_boundary`: the person ended the *step* to take \
                 control and the run stays `Running` (D36) — recorded, never \
                 terminal.",
            ),
        ],
        doc: "A cancellation. Only `reason = run` is terminal; a \
              `takeover_boundary` cancellation is the handover's own step and \
              is followed by `delegate.control.taken`.",
    },
    EventSpec {
        name: "delegate.turn.started",
        version: 1,
        class: EventClass::Domain,
        telemetry: TelemetryPolicy::Never,
        attrs: &[
            id_attr("run", "The run."),
            id_attr("attempt", "The attempt the turn belongs to."),
            id_attr("turn", "The turn."),
            token_attr(
                "originator",
                DELEGATE_ORIGINATOR,
                "Who submitted the turn. Only an orchestrator turn can \
                 complete the objective; a human turn is an intervention.",
            ),
        ],
        doc: "A turn started. The `(attempt, turn, originator)` triple is the \
              identity every turn row carries (§7.5). The actor is the \
              originator: the orchestrator, or the person holding control.",
    },
    EventSpec {
        name: "delegate.turn.completed",
        version: 1,
        class: EventClass::Domain,
        telemetry: TelemetryPolicy::Never,
        attrs: &[
            id_attr("run", "The run."),
            id_attr("attempt", "The attempt."),
            id_attr("turn", "The turn."),
            token_attr("originator", DELEGATE_ORIGINATOR, "Who submitted it."),
            count_attr(
                "artifacts",
                "Artifact versions registered during this turn. Zero on an \
                 orchestrator turn is what makes a run finish needing \
                 attention (D29).",
            ),
        ],
        doc: "A turn completed. **Not necessarily a completed run**: the run \
              ends only when an orchestrator turn completes with nothing \
              pending, and that is `delegate.run.completed`. A human turn's \
              completion is recorded and is not the run's result.",
    },
    EventSpec {
        name: "delegate.turn.failed",
        version: 1,
        class: EventClass::Domain,
        telemetry: TelemetryPolicy::Never,
        attrs: &[
            id_attr("run", "The run."),
            id_attr("attempt", "The attempt."),
            id_attr("turn", "The turn."),
            token_attr("originator", DELEGATE_ORIGINATOR, "Who submitted it."),
            token_attr(
                "stage",
                DELEGATE_FAILURE_STAGE,
                "Where it failed. A stage, never the provider's message.",
            ),
        ],
        doc: "A turn failed. An orchestrator turn's failure fails the run; a \
              human turn's is recorded and never terminalizes (§7.5).",
    },
    EventSpec {
        name: "delegate.turn.cancelled",
        version: 1,
        class: EventClass::Domain,
        telemetry: TelemetryPolicy::Never,
        attrs: &[
            id_attr("run", "The run."),
            id_attr("attempt", "The attempt."),
            id_attr("turn", "The turn."),
            token_attr("originator", DELEGATE_ORIGINATOR, "Who submitted it."),
            token_attr(
                "reason",
                DELEGATE_CANCEL_REASON,
                "Why: the job was ended, or the step was ended so a person \
                 could take control (D36).",
            ),
        ],
        doc: "A turn was cancelled. The actor is the person who cancelled, \
              whichever the reason.",
    },
    EventSpec {
        name: "delegate.tool.completed",
        version: 1,
        class: EventClass::Domain,
        telemetry: TelemetryPolicy::Never,
        attrs: &[
            id_attr("run", "The run."),
            id_attr("attempt", "The attempt."),
            id_attr("turn", "The turn the calls belong to."),
            count_attr("calls", "How many tool calls completed in the turn."),
            count_attr("failed", "How many of them failed."),
            token_attr(
                "outcome",
                DELEGATE_TOOL_OUTCOME,
                "All succeeded, some failed, or all failed — derived from the \
                 two counts, spelled so a surface need not.",
            ),
        ],
        doc: "**Coalesced: one row per turn, at the turn boundary** (ADR-346), \
              counting every `ToolCompleted` the turn produced. Never one row \
              per call, and never a tool's name or argument. An attempt \
              interrupted mid-turn publishes what it had counted.",
    },
    EventSpec {
        name: "delegate.approval.required",
        version: 1,
        class: EventClass::Domain,
        telemetry: TelemetryPolicy::Never,
        attrs: &[
            id_attr("run", "The run."),
            id_attr("attempt", "The attempt."),
            id_attr("turn", "The turn that asked."),
            id_attr("approval", "The approval, by id. Never what it asks."),
        ],
        doc: "The provider asked for an approval (`Running` + attention, D29: \
              M1 answers it with the existing session controls). A pending \
              approval travels with control (§7.4).",
    },
    EventSpec {
        name: "delegate.approval.resolved",
        version: 1,
        class: EventClass::Domain,
        telemetry: TelemetryPolicy::Never,
        attrs: &[
            id_attr("run", "The run."),
            id_attr("attempt", "The attempt."),
            id_attr("turn", "The turn."),
            id_attr("approval", "The approval."),
            token_attr(
                "outcome",
                DELEGATE_APPROVAL_OUTCOME,
                "Approved or rejected by a person, or expired unanswered.",
            ),
        ],
        doc: "An approval was answered or expired. The actor is the person for \
              `approved` and `rejected`; the orchestrator records an `expired` \
              one, because nobody acted.",
    },
    EventSpec {
        name: "delegate.input.waiting",
        version: 1,
        class: EventClass::Domain,
        telemetry: TelemetryPolicy::Never,
        attrs: &[
            id_attr("run", "The run."),
            id_attr("attempt", "The attempt."),
            id_attr("turn", "The turn that is waiting."),
        ],
        doc: "The provider is waiting for input (`Running` + attention). Not \
              terminal, and never what it asked.",
    },
    EventSpec {
        name: "delegate.child_session.detected",
        version: 1,
        class: EventClass::Domain,
        telemetry: TelemetryPolicy::Never,
        attrs: &[
            id_attr("run", "The run."),
            id_attr("attempt", "The attempt."),
            id_attr("child_session", "The child session that appeared."),
            flag_attr(
                "terminated",
                "Whether it was terminated. Native child spawning is disabled \
                 (§7.3), so `true` is the rule and `false` is news.",
            ),
        ],
        doc: "A child session appeared under the run's session and was \
              handled (§7.3: no unmanaged agent hierarchy).",
    },
    EventSpec {
        name: "delegate.attachment.rejected",
        version: 1,
        class: EventClass::Domain,
        telemetry: TelemetryPolicy::Never,
        attrs: &[
            id_attr("run", "The run."),
            id_attr("attempt", "The attempt."),
            id_attr("turn", "The turn."),
            id_attr("attachment", "The attachment, by id. Never its name."),
            token_attr(
                "reason",
                DELEGATE_ATTACHMENT_REJECTION,
                "The session protocol's attachment error class, or `other`.",
            ),
        ],
        doc: "An attachment the run offered was refused (§7.5: rejection is \
              news; acceptance is not).",
    },
    EventSpec {
        name: "delegate.timeline.gap",
        version: 1,
        class: EventClass::Domain,
        telemetry: TelemetryPolicy::Never,
        attrs: &[
            id_attr("run", "The run."),
            id_attr("attempt", "The attempt whose tail was lost."),
            optional(count_attr(
                "events_lost",
                "How many provider events were evicted, when the eviction \
                 reported a count. Absent when it did not — unknown is never \
                 zero.",
            )),
        ],
        doc: "Facts Koru never received — a provider tail evicted from the \
              observation buffer during delegate downtime (§7.1). Replay never \
              invents what it did not receive; it records that it did not.",
    },
    EventSpec {
        name: "delegate.control.taken",
        version: 1,
        class: EventClass::Domain,
        telemetry: TelemetryPolicy::Never,
        attrs: &[
            id_attr("run", "The run."),
            id_attr("attempt", "The attempt whose session the person holds."),
            flag_attr(
                "discovered",
                "`true` when recovery found the takeover already in place \
                 after a crash between release and attach (§7.4); the row is \
                 then timestamped at discovery.",
            ),
        ],
        doc: "The human lease was granted — recorded **only** when it was, \
              never on a request (§7.4). Automatic submission is fenced from \
              here until `delegate.control.returned`; observation stays live. \
              The actor is the person.",
    },
    EventSpec {
        name: "delegate.control.returned",
        version: 1,
        class: EventClass::Domain,
        telemetry: TelemetryPolicy::Never,
        attrs: &[
            id_attr("run", "The run."),
            id_attr("attempt", "The attempt."),
        ],
        doc: "The orchestrator reacquired the session — recorded **only** when \
              reacquisition succeeded (§7.4), whether the person returned it \
              or a window crash released it.",
    },
    EventSpec {
        name: "delegate.artifact.registered",
        version: 1,
        class: EventClass::Domain,
        telemetry: TelemetryPolicy::Never,
        attrs: &[
            id_attr("run", "The run."),
            id_attr("attempt", "The attempt that registered it."),
            optional(id_attr(
                "turn",
                "The turn during which it was registered, when one was active.",
            )),
            id_attr("artifact", "The artifact version (`ArtifactVersionId`)."),
            id_attr("family", "The artifact family the version belongs to."),
            count_attr("ordinal", "The version's ordinal within its family."),
            flag_attr(
                "derived",
                "Whether the version declares a `derived_from` provenance.",
            ),
        ],
        doc: "An artifact version was registered by the run (P1.10). Ids and \
              an ordinal — never the title, the filename or the bytes.",
    },
    EventSpec {
        name: "delegate.artifact.attached",
        version: 1,
        class: EventClass::Domain,
        telemetry: TelemetryPolicy::Never,
        attrs: &[
            id_attr("run", "The run that received it."),
            id_attr("artifact", "The artifact version handed in."),
        ],
        doc: "An earlier result was handed to a run as input — the second half \
              of the roadmap's promise that a result can be used again.",
    },
    EventSpec {
        name: "delegate.turn.usage",
        version: 1,
        class: EventClass::Domain,
        telemetry: TelemetryPolicy::Never,
        attrs: &[
            id_attr("run", "The run."),
            id_attr("attempt", "The attempt — half of the accounting key."),
            id_attr("turn", "The turn — the other half."),
            id_attr(
                "usage",
                "The fact's stable identity (`UsageFactId`): a retry repeats \
                 it, a correction keeps it and raises `revision`.",
            ),
            count_attr(
                "revision",
                "The monotonic amendment revision (§17.2). Replay selects the \
                 highest per key whatever the delivery order.",
            ),
            optional(count_attr(
                "input",
                "Input tokens for the turn. **Absent means unavailable — \
                 never zero.**",
            )),
            optional(count_attr(
                "output",
                "Output tokens. Absent means unavailable.",
            )),
            optional(count_attr(
                "cached_input",
                "Cached input tokens. Absent means unavailable.",
            )),
            token_attr(
                "cached_relationship",
                DELEGATE_CACHED_RELATIONSHIP,
                "Whether `cached_input` is inside `input` or beside it, as the \
                 adapter declared — never assumed.",
            ),
            id_attr(
                "model",
                "A reference to the model metadata retained in the Run \
                 document's `models` map (§17.3). An `Id`, never a name: \
                 resolved by a projection after sync, stripped mechanically \
                 at the telemetry boundary.",
            ),
            token_attr(
                "completeness",
                DELEGATE_COMPLETENESS,
                "Whether a price over this fact would be a guess, and why.",
            ),
        ],
        doc: "One turn's usage, normalized to a total for `(attempt, turn)` \
              (§17.2). Committed in the Run document first, then published \
              (§17.1). **A retry reuses its `event_id`; an amendment gets its \
              own**, so the journal's deduplication keeps retries idempotent \
              without swallowing a correction.",
    },
    EventSpec {
        name: "delegate.run.usage",
        version: 1,
        class: EventClass::Domain,
        telemetry: TelemetryPolicy::Never,
        attrs: &[
            id_attr("run", "The run."),
            token_attr(
                "outcome",
                DELEGATE_RUN_OUTCOME,
                "The terminal status the fold was published under.",
            ),
            count_attr("orchestrator_turns", "Turns the orchestrator submitted."),
            count_attr(
                "human_turns",
                "Turns a person submitted while holding control.",
            ),
            count_attr("turns_with_usage", "Turns with a usage fact after replay."),
            optional(count_attr(
                "input",
                "Input tokens summed over the turns that reported. Absent \
                 when none did — never zero for missing.",
            )),
            count_attr(
                "input_unavailable_turns",
                "Turns whose input count was unavailable.",
            ),
            optional(count_attr(
                "output",
                "Output tokens over the turns that reported; absent when \
                 none did.",
            )),
            count_attr(
                "output_unavailable_turns",
                "Turns whose output count was unavailable.",
            ),
            optional(count_attr(
                "cached_input",
                "Cached input tokens over the turns that reported; absent \
                 when none did.",
            )),
            count_attr(
                "cached_input_unavailable_turns",
                "Turns whose cached-input count was unavailable.",
            ),
            count_attr("tool_calls", "Tool calls over every turn."),
            count_attr(
                "interventions",
                "Control handovers plus human-originated turns (§17.2).",
            ),
            optional(int_attr(
                "elapsed_ms",
                "Wall-clock milliseconds excluding time spent `Interrupted` or \
                 under human control (§17.2). Absent until the run started.",
            )),
            count_attr("models", "How many distinct models any turn used."),
            flag_attr(
                "complete",
                "Whether every usage fact was complete. One incomplete fact \
                 makes the fold incomplete, because a price over it would be \
                 a guess.",
            ),
        ],
        doc: "The run's economics summary (D39): the fold of the replayed \
              usage facts and the timeline, published when the run ends and \
              **re-published under a fresh `event_id` when a late fact amends \
              it** (§17.2: amended, never reopened). A projection of the \
              retained facts, recomputable on any device; the price is \
              computed at display (D40), never here.",
    },
    EventSpec {
        name: "delegate.run.rated",
        version: 1,
        class: EventClass::Domain,
        telemetry: TelemetryPolicy::Never,
        attrs: &[
            id_attr("run", "The run."),
            token_attr("rating", DELEGATE_RATING, "The one gesture (D44)."),
            flag_attr(
                "has_note",
                "Whether a note was written. **The note itself is never an \
                 attribute**: it lives in the run's own content (D44).",
            ),
        ],
        doc: "A person rated a finished run (D42, D44): good or not good, \
              recorded beside the observed outcome so a comparison can tell \
              \"liked it\" from \"it completed\". Optional and never blocking; \
              an unrated run is a normal state and emits nothing.",
    },
    EventSpec {
        name: "delegate.scope.busy",
        version: 1,
        class: EventClass::Domain,
        telemetry: TelemetryPolicy::Never,
        attrs: &[
            id_attr("run", "The run that started anyway."),
            id_attr("holder", "The run that already holds the folder."),
            id_attr("holder_attempt", "That run's live attempt."),
            token_attr(
                "overlap",
                DELEGATE_SCOPE_OVERLAP,
                "How this run's folder relates to the holder's.",
            ),
            count_attr(
                "others",
                "How many further runs also hold an overlapping folder (D34 \
                 lets several).",
            ),
        ],
        doc: "A manual start proceeded onto a busy working folder (D34, \
              P1.5): the person was shown who holds it and went ahead. Ids \
              and the overlap kind — never the folder.",
    },
    EventSpec {
        name: "delegate.scope.skipped",
        version: 1,
        class: EventClass::Domain,
        telemetry: TelemetryPolicy::Never,
        attrs: &[
            id_attr(
                "schedule",
                "The schedule whose firing was skipped. **There is no `run`**: \
                 nothing was minted, which is the point.",
            ),
            id_attr("holder", "The run that holds the folder."),
            id_attr("holder_attempt", "That run's live attempt."),
            token_attr(
                "overlap",
                DELEGATE_SCOPE_OVERLAP,
                "How the schedule's folder relates to the holder's.",
            ),
            count_attr("others", "How many further runs also hold it."),
        ],
        doc: "A scheduled firing was skipped because its folder was busy (D34, \
              P1.5: a schedule never starts onto a held scope). This detailed \
              busy-scope row has no candidate run: its subject is \
              the schedule's own document when one exists, and it starts its \
              own chain. Retained for compatibility; M2's product fact is \
              `delegate.schedule.skipped` and is not dual-emitted with this \
              richer contract.",
    },
    // ---------------------------------------------- delegated schedules (M2)
    //
    // A Schedule is a manifest-registered `koru-schedule` document. Its
    // document id is the subject, its `ScheduleId` is a local-reference attr,
    // and `delegate.schedule.created` is the chain root. Every row is a local
    // domain fact (`Never`). The definition is content: no objective, agent
    // profile, recurrence rule, timezone, scope or context identity appears.
    // Exact occurrence times are local references because they identify a
    // person's activity and must remain unexportable under any future policy.
    EventSpec {
        name: "delegate.schedule.created",
        version: 1,
        class: EventClass::Domain,
        telemetry: TelemetryPolicy::Never,
        attrs: &[
            id_attr("schedule", "The schedule's stable id."),
            id_attr(
                "workspace",
                "The workspace containing the Schedule document. Repeated \
                 because an activity row drops the envelope's scope.",
            ),
            count_attr("revision", "The committed definition revision, from 1."),
        ],
        doc: "The Schedule document was committed — the moment the schedule \
              exists. The chain root: its `event_id` is every later schedule \
              row's `correlation_id`. The actor is whoever created it.",
    },
    EventSpec {
        name: "delegate.schedule.updated",
        version: 1,
        class: EventClass::Domain,
        telemetry: TelemetryPolicy::Never,
        attrs: &[
            id_attr("schedule", "The schedule."),
            count_attr("revision", "The new committed active definition revision."),
        ],
        doc: "A person committed a new active definition or resumed the \
              schedule. The changed objective, provider profile, recurrence, \
              timezone, scope and context remain only in the document.",
    },
    EventSpec {
        name: "delegate.schedule.paused",
        version: 1,
        class: EventClass::Domain,
        telemetry: TelemetryPolicy::Never,
        attrs: &[
            id_attr("schedule", "The schedule."),
            count_attr("revision", "The committed paused definition revision."),
            token_attr(
                "reason",
                DELEGATE_SCHEDULE_PAUSE_REASON,
                "Why execution paused. A closed reason, never an error message.",
            ),
            optional(count_attr(
                "context_item",
                "The zero-based context pointer ordinal, only for \
                 `context_unavailable`. Never the pointed-to identity.",
            )),
            optional(token_attr(
                "stage",
                DELEGATE_SCHEDULE_CONTEXT_STAGE,
                "The content-free context failure stage, only for \
                 `context_unavailable`.",
            )),
        ],
        doc: "The schedule became paused. A manual pause is the person's act; \
              provider, scope and context failures are recorded by the \
              delegate worker. A failed attempt to pause emits nothing.",
    },
    EventSpec {
        name: "delegate.schedule.fired",
        version: 1,
        class: EventClass::Domain,
        telemetry: TelemetryPolicy::Never,
        attrs: &[
            id_attr("schedule", "The schedule."),
            count_attr("revision", "The definition revision that owned the slot."),
            time_attr(
                "occurrence",
                "The planned epoch-millisecond UTC occurrence. Exact because \
                 `(schedule, occurrence)` is the durable slot identity; local \
                 because an exact activity time never leaves the device.",
            ),
            id_attr("run", "The real run created for this occurrence."),
        ],
        doc: "One claimed occurrence created and launched a real run. The run \
              is both the actor's durable identity and the typed `run` attr; \
              its own event chain remains rooted at `delegate.run.created`.",
    },
    EventSpec {
        name: "delegate.schedule.skipped",
        version: 1,
        class: EventClass::Domain,
        telemetry: TelemetryPolicy::Never,
        attrs: &[
            id_attr("schedule", "The schedule."),
            count_attr("revision", "The definition revision that owned the slot."),
            time_attr(
                "occurrence",
                "The planned epoch-millisecond UTC occurrence and durable slot \
                 identity. Local only.",
            ),
            token_attr(
                "reason",
                DELEGATE_SCHEDULE_SKIP_REASON,
                "Whether the slot was missed while asleep or refused because \
                 its scope was busy.",
            ),
            optional(id_attr(
                "holder",
                "The existing run holding the scope, exactly for `busy`.",
            )),
        ],
        doc: "One occurrence was durably recorded without creating a run: a \
              slept-through slot is skipped with zero catch-up (D25), and a \
              busy scope is skipped rather than overlapped (D34). The actor is \
              the delegate worker; a planned skipped RunId is never exposed.",
    },
    // ------------------------------------------- the Store's catalog facts
    //
    // **One producer, one commit point.** Every row below is published by the
    // Store — `koru-store-api`'s handler, after the mutation it reports has
    // committed and the store lock is released — so it covers every writer:
    // this app, Koru Terminal, an MCP tool, a direct API client. They are
    // facts, never commands: the Gateway management operation of ADR-UIREFACTOR
    // §4.2 does its work *before* the event reports it.
    //
    // They are what a window's profile and workspace menus invalidate on
    // (`koru_api_client::events::CatalogWatch`), which is why they are
    // deliberately separate from `workspace.created`/`observed`/`deleted`:
    //
    // - `workspace.created` is the *app's* signal and carries a `source`
    //   (onboarding/manual/import) that only the flow which started it knows;
    //   the Store cannot tell those apart and must not guess.
    // - `workspace.observed` is the events worker's first-sight derivation off
    //   the *projection's* commit feed. It arrives whenever the next sweep
    //   does, not when the catalog committed, and it means "this device had
    //   never seen this workspace" — which a workspace arriving by sync from a
    //   phone also satisfies. Neither its timing nor its meaning is the one a
    //   menu needs.
    //
    // The three answer different questions and are not a triple count of one.
    // `telemetry: Never` throughout: these are local invalidation traffic, and
    // promoting any of them to an exportable policy is a privacy-class review
    // (ADR-166), not an edit.
    EventSpec {
        name: "profile.created",
        version: 1,
        class: EventClass::Domain,
        telemetry: TelemetryPolicy::Never,
        attrs: &[reference_attr(
            "profile",
            "The new profile's id. A profile has no home in the envelope's \
             scope — `scope.profile_id` is the emitting process's, not the \
             record's — so it travels as a local reference.",
        )],
        doc: "The Store committed a new profile record (`POST /v1/profiles`). \
              Emitted once, after the write; a refused or failed create says \
              nothing, because nothing happened. The profile's name, purpose, \
              avatar and locale are deliberately absent: only the id crosses.",
    },
    EventSpec {
        name: "profile.renamed",
        version: 1,
        class: EventClass::Domain,
        telemetry: TelemetryPolicy::Never,
        attrs: &[reference_attr("profile", "The renamed profile's id.")],
        doc: "A profile's display name changed in its replicated manifest — \
              either here (`PATCH /v1/profiles/{id}`) or on another device, \
              arriving through peer or Cloud sync, which emits the same event \
              because a surface has the same work to do either way. Neither \
              the old nor the new name is carried, and neither can be: a \
              person's name for a persona is content, and no AttrKind holds \
              free text.",
    },
    EventSpec {
        name: "profile.appearance_changed",
        version: 1,
        class: EventClass::Domain,
        telemetry: TelemetryPolicy::Never,
        attrs: &[reference_attr("profile", "The profile whose chip changed.")],
        doc: "A profile's switcher icon or named colour changed — either here \
              (`PATCH /v1/profiles/{id}/appearance`) or on another device, \
              arriving through peer or Cloud sync. The icon and the colour \
              name are absent: a banner refetches the record rather than \
              reading it off the bus, which is what keeps this an invalidation \
              signal and not a second copy of the catalog.",
    },
    EventSpec {
        name: "document.private.changed",
        version: 1,
        class: EventClass::Domain,
        telemetry: TelemetryPolicy::Never,
        attrs: &[reference_attr(
            "profile",
            "The reader whose private view state changed.",
        )],
        doc: "One reader's device-local private view state for one document — \
              which toggles they closed — committed a change through \
              `PATCH /v1/profiles/{id}/documents/{document}/private-state`. \
              Workspace-scoped with the document as subject; no node ids and \
              no values. Other windows of the same reader re-read the state. \
              Never replicated and never wakes sync.",
    },
    EventSpec {
        name: "workspace.catalog.created",
        version: 1,
        class: EventClass::Domain,
        telemetry: TelemetryPolicy::Never,
        attrs: &[],
        doc: "The Store committed a new workspace record and its manifest \
              (`POST /v1/workspaces`); subject = the workspace. The Store's own \
              signal, covering every writer — see the note above this block for \
              why it is not `workspace.created` and not `workspace.observed`.",
    },
    EventSpec {
        name: "workspace.catalog.renamed",
        version: 1,
        class: EventClass::Domain,
        telemetry: TelemetryPolicy::Never,
        attrs: &[],
        doc: "A workspace's name changed in the catalog and in its manifest \
              (`PATCH /v1/workspaces/{id}`); subject = the workspace. The name \
              itself never crosses — it is content, and no AttrKind holds it.",
    },
    EventSpec {
        name: "workspace.catalog.appearance_changed",
        version: 1,
        class: EventClass::Domain,
        telemetry: TelemetryPolicy::Never,
        attrs: &[],
        doc: "A committed `PATCH /v1/workspaces/{id}/appearance` — the whole \
              set (icon, colour, pinned, archived) is sent every time, so this \
              is one event per accepted request and never one per field. It is \
              the *invalidation* signal for a switcher; when the request also \
              flipped `archived`, the lifecycle fact rides beside it as \
              `workspace.catalog.archived` or `.unarchived`, because a surface \
              that must act immediately on an archive should not have to \
              refetch to discover it did.",
    },
    EventSpec {
        name: "workspace.catalog.archived",
        version: 1,
        class: EventClass::Domain,
        telemetry: TelemetryPolicy::Never,
        attrs: &[],
        doc: "A workspace became archived — the false-to-true transition only, \
              read from the record before and after the same committed \
              appearance write. Re-sending `archived: true` for a workspace \
              that already was says nothing, because nothing changed. Archiving \
              hides a workspace and its projects; it mutates no project record.",
    },
    EventSpec {
        name: "workspace.catalog.unarchived",
        version: 1,
        class: EventClass::Domain,
        telemetry: TelemetryPolicy::Never,
        attrs: &[],
        doc: "The same transition the other way. Deliberately not \
              `workspace.catalog.restored`: that one is the end of a soft \
              delete, and archive and soft-delete are independent axes on the \
              record (`archived: bool`, `deleted_at: Option<Timestamp>`), not \
              two names for one state.",
    },
    EventSpec {
        name: "workspace.catalog.deleted",
        version: 1,
        class: EventClass::Domain,
        telemetry: TelemetryPolicy::Never,
        attrs: &[],
        doc: "The Store soft-deleted a workspace (`DELETE /v1/workspaces/{id}`) \
              — recoverable until the retention window purges it; subject = the \
              workspace. Emitted only when the delete committed: a request \
              refused because live connections hold the workspace emits nothing.",
    },
    EventSpec {
        name: "workspace.catalog.restored",
        version: 1,
        class: EventClass::Domain,
        telemetry: TelemetryPolicy::Never,
        attrs: &[],
        doc: "A soft-deleted workspace came back (`POST \
              /v1/workspaces/{id}/restore`); subject = the workspace. Its \
              hidden projects reappear with no project mutation.",
    },
    EventSpec {
        name: "workspace.catalog.moved",
        version: 1,
        class: EventClass::Domain,
        telemetry: TelemetryPolicy::Never,
        attrs: &[
            reference_attr(
                "from_profile",
                "The profile that owned the workspace until this commit. Carried \
                 because a consumer repairing state keyed on the old owner — a \
                 project's typed scope, a per-profile listing — needs to know \
                 which key stopped being true, and the envelope holds only the \
                 workspace.",
            ),
            reference_attr(
                "to_profile",
                "The profile that owns it now. Both ids travel because a fact \
                 that named only the destination would leave a late subscriber \
                 unable to tell a move apart from a create.",
            ),
        ],
        doc: "A workspace's owning profile changed (`POST \
              /v1/workspaces/{id}/move`); subject = the workspace. Its id, its \
              name, its documents and its history are untouched — only the \
              owner and its position in the destination's order. Emitted only \
              when the reassignment committed; a request refused because the \
              target profile does not exist emits nothing.",
    },
    EventSpec {
        name: "workspace.catalog.imported",
        version: 1,
        class: EventClass::Domain,
        telemetry: TelemetryPolicy::Never,
        attrs: &[token_attr(
            "mode",
            &["restore", "duplicate"],
            "Which of the two import modes ran. `restore` keeps the bundle's \
             ids so the result can converge with its origin; `duplicate` mints \
             new ones so it never will. There is no default, so there is no \
             third token.",
        )],
        doc: "A `.koru` bundle was imported and its workspace record committed \
              (`POST /v1/import`); subject = the resulting workspace. Kept \
              apart from `workspace.catalog.created` because the two answer \
              different questions — a restored workspace is not a new one — and \
              because `mode` is a fact only this route knows.",
    },
    EventSpec {
        name: "workspace.catalog.reordered",
        version: 1,
        class: EventClass::Domain,
        telemetry: TelemetryPolicy::Never,
        attrs: &[reference_attr(
            "profile",
            "Whose order changed. The workspaces are not listed: the new order \
             is read back from the catalog, never off the bus.",
        )],
        doc: "A profile's complete workspace order was replaced atomically \
              (`PUT /v1/profiles/{id}/workspace-order`). There is no workspace \
              subject because every workspace the profile owns may have moved.",
    },
    // ---------------------------------------- the settings authority's facts
    //
    // **One producer, and it is the only writer of the settings files.** The
    // authority publishes after the atomic write has returned, so a consumer
    // that reacts to one of these and refetches is guaranteed to read the
    // value the fact was about. A refused or failed write says nothing,
    // because nothing happened.
    //
    // **A section token, and nothing else.** No key, no old value, no new
    // value. A settings fact is an invalidation: what somebody's fonts,
    // microphone or startup behavior actually are is theirs, and a bus is the
    // wrong place to learn it even locally. A surface refetches the section
    // from the authority that just committed it.
    //
    // Three rows rather than one with a section attr, so a consumer that cares
    // about one section can filter by name — the `workspace.*` precedent — and
    // the attr carries the same token so a handler that wants all three can
    // read it uniformly.
    EventSpec {
        name: "settings.global_changed",
        version: 1,
        class: EventClass::Domain,
        telemetry: TelemetryPolicy::Never,
        attrs: &[token_attr(
            "section",
            &["global", "ai", "terminal"],
            "Which section committed. Redundant with the event name by \
             design: a handler subscribed to all three reads one field \
             instead of re-parsing a name.",
        )],
        doc: "The settings authority committed a change to the Global \
              section (`PATCH /v1/settings/global`), and the bytes are on the \
              disk. Emitted once, after the atomic write returns. Carries no \
              value: a window invalidates and refetches the section.",
    },
    EventSpec {
        name: "settings.ai_changed",
        version: 1,
        class: EventClass::Domain,
        telemetry: TelemetryPolicy::Never,
        attrs: &[token_attr(
            "section",
            &["global", "ai", "terminal"],
            "Which section committed.",
        )],
        doc: "The settings authority committed a change to the AI section \
              (`PATCH /v1/settings/ai`). The section is a facade over the \
              existing AI authority, so the write landed in `ai.db`; the \
              secret vault is never involved and is never reported. Carries \
              no value.",
    },
    EventSpec {
        name: "settings.terminal_changed",
        version: 1,
        class: EventClass::Domain,
        telemetry: TelemetryPolicy::Never,
        attrs: &[token_attr(
            "section",
            &["global", "ai", "terminal"],
            "Which section committed.",
        )],
        doc: "The settings authority committed a change to the Terminal \
              section (`PATCH /v1/settings/terminal`), and the bytes are on \
              the disk. Carries no value.",
    },
    EventSpec {
        name: "app.onboarding.entry_selected",
        version: 1,
        class: EventClass::Instrumentation,
        telemetry: TelemetryPolicy::AnonymousUsage,
        attrs: &[token_attr(
            "choice",
            &["offline", "google", "apple"],
            "Whether the person chose local-only setup, Google sign-in or Sign in with Apple.",
        )],
        doc: "The person chose the first-run path on the desktop Welcome screen. Emitted once per explicit choice, before Profile creation; carries only the closed path category.",
    },
    EventSpec {
        name: "app.onboarding.profile_purpose_selected",
        version: 1,
        class: EventClass::Instrumentation,
        telemetry: TelemetryPolicy::AnonymousUsage,
        attrs: &[token_attr(
            "purpose",
            &["work", "school", "personal"],
            "The selected Profile purpose card.",
        )],
        doc: "The person selected a Profile purpose during first-run onboarding. Emitted for each explicit card selection so changed choices remain observable; carries no name, Profile id, or content.",
    },
    EventSpec {
        name: "app.installed",
        version: 1,
        class: EventClass::Domain,
        telemetry: TelemetryPolicy::AnonymousUsage,
        attrs: &[count_attr(
            "build",
            "The build number of the install that first ran (0 for a dev build).",
        )],
        doc: "The very first run of this installation — emitted exactly once, \
              by the events worker when its journal is first created (one \
              journal is one channel installation's, so this is once per \
              channel install; a cleared store therefore counts as a fresh \
              install, which is the honest reading with no durable identity \
              to say otherwise).",
    },
    EventSpec {
        name: "app.update.available",
        version: 1,
        class: EventClass::Domain,
        telemetry: TelemetryPolicy::AnonymousUsage,
        attrs: &[to_build_attr()],
        doc: "The agent's check found a newer build and offered it. Once per \
              offered build, not per check — the same offer repeating is not \
              news. The running build is the telemetry batch's app_version \
              dimension, so offered-vs-running needs no second attr.",
    },
    EventSpec {
        name: "app.update.downloaded",
        version: 1,
        class: EventClass::Domain,
        telemetry: TelemetryPolicy::AnonymousUsage,
        attrs: &[to_build_attr()],
        doc: "The offered build was downloaded and verified (stage Ready). \
              With `available` and `accepted` this is the upgrade funnel: \
              where offers stall is the gap between these events.",
    },
    EventSpec {
        name: "app.update.accepted",
        version: 1,
        class: EventClass::Domain,
        telemetry: TelemetryPolicy::AnonymousUsage,
        attrs: &[to_build_attr()],
        doc: "The person consented to install (the second consent of \
              ADR-113). The install itself follows; `app.updated` is the far \
              side saying it worked.",
    },
    EventSpec {
        name: "app.updated",
        version: 1,
        class: EventClass::Domain,
        telemetry: TelemetryPolicy::AnonymousUsage,
        attrs: &[count_attr(
            "build",
            "The build now running — the one the update installed.",
        )],
        doc: "An update completed and the new build is running — emitted by \
              the agent on its first start after a successful install (the \
              health-token launch, the same evidence What's new uses). An \
              `accepted` with no `updated` after it is an install that \
              failed or rolled back.",
    },
    EventSpec {
        name: "app.environment",
        version: 1,
        class: EventClass::Instrumentation,
        telemetry: TelemetryPolicy::AnonymousUsage,
        attrs: &[
            token_attr(
                "os",
                &["windows", "macos", "linux", "other"],
                "Which platform family.",
            ),
            optional(AttrSpec {
                key: "os_version",
                kind: AttrKind::Version,
                class: FieldClass::ExactCategorical,
                required: true,
                vocabulary: &[],
                doc: "The platform's own version, dotted digits (`10.0.26200`).",
            }),
            count_attr(
                "app_build",
                "This build's number; 0 for a dev build. The version string \
                 is a telemetry batch dimension already — this is the exact \
                 local build.",
            ),
            token_attr(
                "system_language",
                LANGUAGE_TOKENS,
                "The OS's language, mapped to a supported language or `other` \
                 locally — the raw tag is never sent.",
            ),
            token_attr(
                "app_language",
                LANGUAGE_TOKENS,
                "The language Koru is drawing in (ADR-87: the nearest \
                 supported to the person's).",
            ),
            optional(AttrSpec {
                key: "utc_offset_minutes",
                kind: AttrKind::Int,
                class: FieldClass::ExactCategorical,
                required: true,
                vocabulary: &[],
                doc: "The machine's offset from UTC in minutes, signed. The \
                      zone *name* is deliberately not sent: the offset is what \
                      a calendar decision needs and it is far coarser.",
            }),
            optional(count_attr(
                "display_width",
                "The primary display's width in physical pixels. Exact rather \
                 than bucketed: the value's cardinality is a few dozen common \
                 resolutions, and a layout decision needs the real number.",
            )),
            optional(count_attr(
                "display_height",
                "The primary display's height in physical pixels.",
            )),
            optional(count_attr(
                "display_scale_percent",
                "The primary display's scale factor as a percentage (100, \
                 125, 150, 200) — what fractional DPI arrives as.",
            )),
            optional(count_attr(
                "monitor_count",
                "How many displays the machine reports.",
            )),
            optional(count_attr(
                "window_width",
                "The main window's width in logical pixels at startup.",
            )),
            optional(count_attr(
                "window_height",
                "The main window's height in logical pixels at startup.",
            )),
            optional(bucket_attr(
                "ram_gb_bucket",
                "Installed memory in gigabytes, bucketed.",
            )),
            optional(bucket_attr(
                "disk_free_gb_bucket",
                "Free space on the volume holding this channel's data, \
                 bucketed — the dimension an indexing or update failure is \
                 read against.",
            )),
            optional(token_attr(
                "keyboard_layout",
                &["qwerty", "azerty", "qwertz", "other"],
                "The layout family, derived from the active layout — the \
                 dimension a keyboard-shortcut decision needs.",
            )),
            token_attr(
                "cpu_arch",
                &["x86_64", "aarch64", "other"],
                "The architecture this build is running as.",
            ),
            optional(count_attr(
                "cpu_count",
                "Logical processors available to this process.",
            )),
            optional(token_attr(
                "gpu_vendor",
                &["nvidia", "amd", "intel", "apple", "microsoft", "other"],
                "The rendering adapter's vendor family, from the adapter name.",
            )),
            optional(token_attr(
                "gpu_backend",
                &["vulkan", "dx12", "metal", "gl", "browser_webgpu", "other"],
                "Which wgpu backend is drawing.",
            )),
            optional(flag_attr(
                "touch",
                "Whether the machine reports a touch digitizer.",
            )),
            optional(token_attr(
                "theme",
                &["light", "dark"],
                "The system appearance Koru opened in.",
            )),
            optional(flag_attr(
                "reduced_motion",
                "Whether the OS asks for reduced animation.",
            )),
            optional(flag_attr(
                "high_contrast",
                "Whether a high-contrast scheme is on.",
            )),
            optional(count_attr(
                "text_scale_percent",
                "The OS accessibility text scale, as a percentage; 100 when \
                 unscaled.",
            )),
            optional(token_attr(
                "first_day_of_week",
                &["monday", "sunday", "saturday", "other"],
                "Which day the locale starts a week on — a calendar decision.",
            )),
            optional(token_attr(
                "clock",
                &["h12", "h24"],
                "Whether the locale prefers a 12- or 24-hour clock.",
            )),
            optional(token_attr(
                "network",
                &["online", "offline", "unknown"],
                "Whether the machine believed it had connectivity at startup.",
            )),
        ],
        doc: "The machine and its settings, once per application run, right \
              after the first frame. Every value is a dimension a product or \
              rendering decision is read against — never a name, an address, \
              a serial, or anything about the person. Optional attrs are \
              absent when the platform did not answer, which is itself \
              honest: absence means 'not determined', never zero. Collected \
              off the frame loop.",
    },
    EventSpec {
        name: "app.session.started",
        version: 1,
        class: EventClass::Instrumentation,
        telemetry: TelemetryPolicy::AnonymousUsage,
        attrs: &[token_attr(
            "previous_session_clean",
            &["clean", "unclean", "unknown"],
            "Whether the previous run of this app ended cleanly — the \
             crash signal, reported on the NEXT startup because a crashing \
             process cannot report its own.",
        )],
        doc: "An application run began. One per process start.",
    },
    EventSpec {
        name: "app.session.ended",
        version: 1,
        class: EventClass::Instrumentation,
        telemetry: TelemetryPolicy::AnonymousUsage,
        attrs: &[bucket_attr(
            "duration_bucket",
            "The run's length, bucketed client-side (crate::bucket over \
             whole minutes).",
        )],
        doc: "An application run ended cleanly. Absent after a crash — which \
              is what previous_session_clean on the next start reports.",
    },
    EventSpec {
        name: "search.opened",
        version: 1,
        class: EventClass::Instrumentation,
        telemetry: TelemetryPolicy::AnonymousUsage,
        attrs: &[token_attr(
            "surface",
            &["overlay", "window", "in_app"],
            "Which search surface was opened.",
        )],
        doc: "A search surface was opened. Deliberately the ONLY search \
              event, and it has no query attr — none can be declared, which \
              is what enforces the rule that query text never enters \
              settings, the index or telemetry (docs/features/search/README.md).",
    },
    EventSpec {
        name: "import.completed",
        version: 1,
        class: EventClass::Domain,
        telemetry: TelemetryPolicy::AnonymousUsage,
        attrs: &[
            token_attr(
                "source",
                &[
                    "markdown",
                    "textbundle",
                    "onenote",
                    "legacy_workspace",
                    "files",
                    "google_docs",
                    "notion",
                    "evernote",
                ],
                "Which importer ran.",
            ),
            bucket_attr(
                "document_count_bucket",
                "How many documents it produced, bucketed.",
            ),
            token_attr(
                "outcome",
                &["ok", "partial", "failed"],
                "Whether everything, some, or nothing was imported — the \
                 friction signal.",
            ),
        ],
        doc: "An import run finished, however it went. One per run, emitted \
              by the importer host.",
    },
    EventSpec {
        name: "voice.dictation.completed",
        version: 1,
        class: EventClass::Instrumentation,
        telemetry: TelemetryPolicy::AnonymousUsage,
        attrs: &[
            token_attr(
                "outcome",
                &["committed", "cancelled", "discarded", "failed"],
                "How the dictation ended. `committed` is the one that wrote \
                 text; `cancelled` is the person's own Escape; `discarded` is \
                 an interruption throwing the provisional text away (focus \
                 lost, the window closed, the machine slept); `failed` is the \
                 engine or the device. The four are the whole outcome space \
                 and a dictation reports exactly one.",
            ),
            token_attr(
                "surface",
                &["document", "field"],
                "Which kind of editable surface received it — a document body \
                 or a one-line field. Not which document and not which field: \
                 the two implementations differ and nothing finer is a \
                 product question.",
            ),
            token_attr(
                "language",
                LANGUAGE_TOKENS,
                "The language the recogniser was asked for, mapped to a \
                 supported language or `other` locally. `Requested::Auto` \
                 reports the language that was *detected*, because a request \
                 that names none still produced text in one.",
            ),
            token_attr(
                "engine",
                VOICE_ENGINE_TOKENS,
                "Which speech runtime ran — see VOICE_ENGINE_TOKENS for what \
                 changes the set.",
            ),
            bucket_attr(
                "duration_bucket",
                "How long the dictation ran, in whole SECONDS, through \
                 crate::bucket. Seconds rather than the milliseconds the plan \
                 first named: the ladder tops out at 10 000, so every \
                 dictation over ten seconds would share one rung and the \
                 signal would be gone.",
            ),
        ],
        doc: "A dictation ended, however it ended. One per session, emitted \
              by the host that owned the composition. **There is deliberately \
              no transcript attr, and none can be declared** — the same rule \
              search.opened holds for query text, and structural for the same \
              reason: no AttrKind carries free text, so spoken words have no \
              representation here to be leaked through, forgotten about or \
              widened later. Per ADR-166 audio, transcripts and per-token \
              confidences are content; content is unrepresentable in this \
              registry rather than merely discouraged. A count of anything \
              measured off the words — characters, tokens, revisions — is the \
              same mistake one step removed and is not declared either.",
    },
    EventSpec {
        name: "voice.model.installed",
        version: 1,
        class: EventClass::Domain,
        telemetry: TelemetryPolicy::AnonymousUsage,
        attrs: &[
            token_attr(
                "tier",
                VOICE_TIER_TOKENS,
                "The quality tier the person chose — koru_voice_models::\
                 ModelTier. It stands in for the model *name*, which is an \
                 open remote vocabulary and therefore not declarable; see \
                 VOICE_TIER_TOKENS.",
            ),
            token_attr(
                "engine",
                VOICE_ENGINE_TOKENS,
                "Which runtime the model is for, so a tier is read against \
                 the engine that will run it.",
            ),
            AttrSpec {
                key: "version",
                kind: AttrKind::Version,
                class: FieldClass::ExactCategorical,
                required: true,
                vocabulary: &[],
                doc: "The model descriptor's own version, dotted digits \
                      (`1.2.0`) — which entry of the catalog this is. With \
                      engine and tier it names the download; on its own it \
                      names nothing.",
            },
            bucket_attr(
                "size_mb_bucket",
                "The installed size in megabytes, bucketed — the dimension a \
                 download-abandonment or disk-space question is read against. \
                 Never a path, and never the cache directory.",
            ),
        ],
        doc: "A speech model finished downloading, verifying and installing, \
              and is usable. One per successful install; a re-install of the \
              same version after a cache clear says it again, which is \
              honest — the bytes were fetched twice. A failed or abandoned \
              download says nothing here. The model *id* is absent on \
              purpose: the catalog is remote, versioned and open to \
              third-party entries, so a closed vocabulary for it does not \
              exist and inventing one would freeze a list that lives on a \
              server.",
    },
    EventSpec {
        name: "account.operation.completed",
        version: 1,
        class: EventClass::Instrumentation,
        telemetry: TelemetryPolicy::Operational,
        attrs: &[
            token_attr(
                "client",
                &["web", "native", "service"],
                "The closed caller family; never a user agent, application id, or source address.",
            ),
            token_attr(
                "provider",
                &["google", "email", "apple", "none"],
                "The configured identity-provider family, or none for provider-independent work.",
            ),
            token_attr(
                "platform",
                &[
                    "web", "windows", "macos", "ios", "android", "other", "unknown",
                ],
                "The client platform mapped locally to a closed category; unknown is used when the request does not carry one.",
            ),
            token_attr(
                "stage",
                &[
                    "auth_start",
                    "auth_callback",
                    "session_exchange",
                    "session_refresh",
                    "session_logout",
                    "identity_link",
                    "profile_create",
                    "profile_attach",
                    "profile_rebind",
                    "profile_delete",
                    "profile_restore",
                    "device_register",
                    "enrollment",
                ],
                "The consequential operation that reached a terminal result; internal steps are debug diagnostics, not events.",
            ),
            token_attr(
                "outcome",
                &["succeeded", "rejected", "failed"],
                "Whether the operation committed, was explicitly rejected, or failed operationally.",
            ),
            token_attr(
                "failure",
                &[
                    "none",
                    "invalid_request",
                    "unauthenticated",
                    "permission_denied",
                    "capability_unavailable",
                    "rate_limited",
                    "flow_expired",
                    "flow_consumed",
                    "provider_rejected",
                    "account_mismatch",
                    "identity_collision",
                    "profile_already_bound",
                    "device_already_bound",
                    "conflict",
                    "not_found",
                    "server_unavailable",
                    "timeout",
                    "integrity",
                    "other",
                ],
                "The stable failure class, or none. Provider text and source errors never enter this vocabulary.",
            ),
            bucket_attr(
                "duration_ms_bucket",
                "End-to-end operation time in milliseconds, bucketed before the event is built.",
            ),
        ],
        doc: "A consequential account authentication, session, Profile/device mutation, or enrollment operation reached one terminal outcome. Reads and internal stages do not emit it. The payload contains only closed categories and a client-side duration bucket: email, provider subject, Profile name, token, state, nonce, full URL, user agent, and raw source identifier have no field.",
    },
    EventSpec {
        name: "terminal.surface.opened",
        version: 1,
        class: EventClass::Instrumentation,
        telemetry: TelemetryPolicy::AnonymousUsage,
        attrs: &[token_attr(
            "surface",
            &["terminal"],
            "The reviewed host surface. New hosts add a closed token here before emitting.",
        )],
        doc: "A terminal-session surface opened. Exactly once per Koru Terminal \
              window; a future embedded host must add its reviewed surface token \
              before it emits this event.",
    },
    EventSpec {
        name: "terminal.session.started",
        version: 1,
        class: EventClass::Instrumentation,
        telemetry: TelemetryPolicy::AnonymousUsage,
        attrs: &[
            token_attr("mode", &["raw", "managed"], "The session execution mode."),
            token_attr(
                "profile_kind",
                &["shell", "agent", "custom"],
                "The closed launch-profile category, never its display name or argv.",
            ),
            token_attr(
                "provider_kind",
                &[
                    "none", "claude", "opencode", "codex", "grok", "kimi", "other",
                ],
                "The reviewed provider category. Unknown provider tokens collapse to other locally.",
            ),
        ],
        doc: "A raw or managed session actually started. Exactly once after \
              the runtime commits the transition to running; an attempted \
              launch that never starts does not count.",
    },
    EventSpec {
        name: "terminal.session.reattached",
        version: 1,
        class: EventClass::Instrumentation,
        telemetry: TelemetryPolicy::AnonymousUsage,
        attrs: &[],
        doc: "A detached live session gained a controller. Once per committed \
              detached-to-controlled transition, never for an idempotent repeat.",
    },
    EventSpec {
        name: "terminal.session.ended",
        version: 1,
        class: EventClass::Instrumentation,
        telemetry: TelemetryPolicy::AnonymousUsage,
        attrs: &[
            token_attr(
                "outcome",
                &["exited", "failed", "terminated"],
                "The final client-visible lifecycle outcome.",
            ),
            bucket_attr(
                "duration_minutes_bucket",
                "The session lifetime in whole minutes, bucketed on the client.",
            ),
        ],
        doc: "A session reached its final exit, launch/runtime failure, or \
              explicit termination. Exactly once at the terminal transition.",
    },
    EventSpec {
        name: "terminal.input.submitted",
        version: 1,
        class: EventClass::Instrumentation,
        telemetry: TelemetryPolicy::AnonymousUsage,
        attrs: &[token_attr(
            "input_kind",
            &["text", "image", "mixed"],
            "The logical pending-input shape; never its text, bytes, or attachment names.",
        )],
        doc: "One logical pending-input submission was accepted for delivery. \
              Once per submission, never per key, byte, token, paste character, \
              or attachment chunk.",
    },
    EventSpec {
        name: "terminal.attachment.delivered",
        version: 1,
        class: EventClass::Instrumentation,
        telemetry: TelemetryPolicy::AnonymousUsage,
        attrs: &[
            token_attr(
                "delivery_mode",
                &["native", "path", "unsupported"],
                "The provider-neutral delivery route that completed.",
            ),
            token_attr(
                "outcome",
                &["delivered", "failed", "unsupported"],
                "The categorical terminal delivery result.",
            ),
        ],
        doc: "An attachment delivery attempt reached a final result. Once per \
              completed native, safely quoted path, or explicit unsupported \
              delivery; size, path, filename, media type, and content are absent.",
    },
    EventSpec {
        name: "terminal.experience.summary",
        version: 1,
        class: EventClass::Instrumentation,
        telemetry: TelemetryPolicy::AnonymousUsage,
        attrs: &[
            optional(bucket_attr(
                "first_present_ms_bucket",
                "First presentation latency in milliseconds, bucketed on the client.",
            )),
            optional(bucket_attr(
                "switch_present_ms_bucket",
                "Session-switch-to-presentation latency in milliseconds, bucketed on the client.",
            )),
            optional(bucket_attr(
                "delta_present_p95_ms_bucket",
                "The sampled delta-to-presentation p95 in milliseconds, bucketed on the client.",
            )),
            optional(bucket_attr(
                "render_cpu_p95_ms_bucket",
                "The sampled render CPU p95 in milliseconds, bucketed on the client.",
            )),
            optional(bucket_attr(
                "jank_per_thousand_bucket",
                "Janky samples per thousand presentation samples, bucketed on the client.",
            )),
            optional(bucket_attr(
                "stream_gap_count_bucket",
                "Stream gaps in this summary window, bucketed on the client.",
            )),
            optional(bucket_attr(
                "sample_count_bucket",
                "Presentation samples in this summary window, bucketed on the client.",
            )),
        ],
        doc: "A bounded client-side experience rollup. At most once per 15 \
              accumulated visible minutes, emitted opportunistically by the next \
              presentation sample, plus one detach/end flush only when unsent \
              samples remain. It has no timer; hidden time does not accrue and \
              missing measurements remain absent rather than becoming zero.",
    },
    EventSpec {
        name: "terminal.project.created",
        version: 1,
        class: EventClass::Domain,
        telemetry: TelemetryPolicy::AnonymousUsage,
        attrs: &[
            token_attr(
                "scope",
                &["workspace", "unscoped"],
                "Whether the project was created inside a workspace scope or with none.",
            ),
            token_attr(
                "root",
                &["set", "unset"],
                "Whether a folder was chosen when the project was created.",
            ),
        ],
        doc: "A terminal project was created. Emitted once by the session host \
              after the registry write commits, never on a replayed create. \
              The project's name and folder are deliberately absent: only the \
              two closed tokens above cross this boundary.",
    },
    EventSpec {
        name: "terminal.project.deleted",
        version: 1,
        class: EventClass::Domain,
        telemetry: TelemetryPolicy::AnonymousUsage,
        attrs: &[token_attr(
            "scope",
            &["workspace", "unscoped"],
            "Whether the deleted project had a workspace scope.",
        )],
        doc: "A terminal project was deleted. Emitted once by the session host \
              after the registry write commits; a deletion refused because the \
              project still had running sessions emits nothing, because nothing \
              happened. Deleting a project ends no process.",
    },
    EventSpec {
        name: "terminal.tab.opened",
        version: 1,
        class: EventClass::Domain,
        telemetry: TelemetryPolicy::AnonymousUsage,
        attrs: &[token_attr(
            "kind",
            &["welcome", "session", "settings"],
            "Which of the three tab kinds was opened.",
        )],
        doc: "A tab was opened in a Koru Terminal window. One closed token and \
              nothing else: no title, no session id, no profile and no project. \
              The floor a window opens on counts — a Welcome tab is a tab — and \
              so does the one a never-empty strip mints when its last tab \
              closes; selecting a tab that already exists does not, because \
              nothing was opened.",
    },
];

/// The entry for a name, when it has one.
pub fn spec(name: &str) -> Option<&'static EventSpec> {
    REGISTRY.iter().find(|spec| spec.name == name)
}

/// Const-evaluable membership test — what [`crate::event_name!`] rides.
pub const fn is_registered(name: &str) -> bool {
    let mut index = 0;
    while index < REGISTRY.len() {
        if str_eq(REGISTRY[index].name, name) {
            return true;
        }
        index += 1;
    }
    false
}

const fn str_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut index = 0;
    while index < a.len() {
        if a[index] != b[index] {
            return false;
        }
        index += 1;
    }
    true
}

/// The name grammar: lowercase dotted segments, `[a-z][a-z0-9_]*`, at least
/// two of them, at most 64 characters, first segment a registered area.
pub fn name_is_well_formed(name: &str) -> bool {
    if name.len() > 64 {
        return false;
    }
    let mut segments = name.split('.');
    let Some(area) = segments.next() else {
        return false;
    };
    if !AREAS.contains(&area) {
        return false;
    }
    let mut rest = 0;
    for segment in segments {
        rest += 1;
        let mut chars = segment.chars();
        match chars.next() {
            Some(first) if first.is_ascii_lowercase() => {}
            _ => return false,
        }
        if !chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_') {
            return false;
        }
    }
    rest >= 1
}

/// A literal event name, checked against [`REGISTRY`] when the crate
/// compiles: a name missing from the table is a build error at the call
/// site, not a runtime refusal.
#[macro_export]
macro_rules! event_name {
    ($name:literal) => {{
        const _: () = assert!(
            $crate::registry::is_registered($name),
            concat!("`", $name, "` is not declared in koru_events::REGISTRY — add a row (and its doc) before publishing it"),
        );
        $crate::EventName::from_static($name)
    }};
}
