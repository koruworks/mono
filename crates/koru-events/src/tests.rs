// SPDX-License-Identifier: MPL-2.0

use std::collections::BTreeMap;

use uuid::Uuid;

use super::*;
use crate::registry::{AREAS, name_is_well_formed};
use crate::telemetry::{Consent, project};

// ------------------------------------------------------------- the registry

#[test]
fn every_name_appears_exactly_once() {
    for spec in REGISTRY {
        assert_eq!(
            REGISTRY.iter().filter(|s| s.name == spec.name).count(),
            1,
            "{} is declared twice",
            spec.name
        );
    }
}

#[test]
fn every_name_matches_the_grammar() {
    for spec in REGISTRY {
        assert!(
            name_is_well_formed(spec.name),
            "{} does not match `area.segment[.segment]` over {:?}",
            spec.name,
            AREAS
        );
    }
    assert!(!name_is_well_formed("document"));
    assert!(!name_is_well_formed("Document.created"));
    assert!(!name_is_well_formed("kitchen.sink"));
    assert!(!name_is_well_formed("document.Created"));
    assert!(!name_is_well_formed("document..created"));
}

#[test]
fn every_entry_says_what_it_is_for() {
    for spec in REGISTRY {
        assert!(!spec.doc.trim().is_empty(), "{} has no doc", spec.name);
        for attr in spec.attrs {
            assert!(
                !attr.doc.trim().is_empty(),
                "{}.{} has no doc",
                spec.name,
                attr.key
            );
        }
    }
}

/// The "Acquisition of Acme Corp" failure dies here, not in review: no attr
/// key that *could* hold a string may even smell like it might carry
/// content.
///
/// Scoped to the string-carrying kinds, because that is where the hazard
/// lives: a `Token` is a closed vocabulary and a `Version` is digits and
/// dots, so both are already narrow — but a key called `title` on either
/// would still be somebody about to widen one. A `Count` named
/// `text_scale_percent` cannot carry text whatever it is called, and
/// refusing it would be the guard mistaking a word for a risk.
#[test]
fn no_attr_key_smells_like_content() {
    const SMELLS: &[&str] = &[
        "title", "text", "query", "path", "url", "content", "body", "message", "email", "filename",
        "prompt", "response",
    ];
    for spec in REGISTRY {
        for attr in spec.attrs {
            if !matches!(attr.kind, AttrKind::Token | AttrKind::Version) {
                continue;
            }
            for smell in SMELLS {
                assert!(
                    !attr.key.contains(smell),
                    "{}.{} contains `{}` — an attr key must not name content",
                    spec.name,
                    attr.key,
                    smell
                );
            }
        }
    }
}

/// **Speech content is unrepresentable, not discouraged** (ADR-166).
///
/// The rule above is scoped to the kinds that carry a string, because that is
/// where the "Acquisition of Acme Corp" hazard lives. This one is scoped to
/// *every* kind, because the hazard is different: a `Count` called
/// `transcript_chars` or a `Bucket` called `confidence` carries no words and is
/// still a measurement of what somebody said, and the second one is how the
/// first gets argued back in. Neither audio nor a transcript nor a per-token
/// confidence has a representation in this registry — `AttrKind` has no free
/// text and no float — so this test guards the names, and the absence of a kind
/// guards the values.
#[test]
fn no_attr_may_name_speech_content() {
    // `sample` was on this list until the terminal experience summary counted
    // its *timing* samples (`sample_count_bucket`) — a sample of a duration is
    // not a sample of a voice. Audio-sample leakage stays blocked by `audio`
    // and `waveform`.
    const FORBIDDEN: &[&str] = &[
        "transcript",
        "audio",
        "utterance",
        "waveform",
        "confidence",
        "phoneme",
        "spoken",
        "dictated",
        "device_name",
    ];
    for spec in REGISTRY {
        for attr in spec.attrs {
            for word in FORBIDDEN {
                assert!(
                    !attr.key.contains(word),
                    "{}.{} contains `{}` — audio, transcripts and per-token confidences are content (ADR-166) and cannot be declared, in any kind",
                    spec.name,
                    attr.key,
                    word
                );
            }
        }
    }
}

/// `ExactCategorical` claims the vocabulary was cardinality-reviewed, so the
/// vocabulary has to be reviewable: no duplicates (two rows for one value is a
/// merge that was never noticed) and small enough that a person reads it in
/// one go. Thirty-two is not tuning — it is far above every declared set and
/// far below the point at which "closed vocabulary" stops meaning anything.
#[test]
fn every_vocabulary_is_reviewable() {
    for spec in REGISTRY {
        for attr in spec.attrs {
            if attr.vocabulary.is_empty() {
                continue;
            }
            let unique: std::collections::BTreeSet<&str> =
                attr.vocabulary.iter().copied().collect();
            assert_eq!(
                unique.len(),
                attr.vocabulary.len(),
                "{}.{} declares a token twice",
                spec.name,
                attr.key
            );
            assert!(
                attr.vocabulary.len() <= 32,
                "{}.{} declares {} tokens — a set nobody can read is not a reviewed one",
                spec.name,
                attr.key,
                attr.vocabulary.len()
            );
        }
    }
}

#[test]
fn vocabularies_belong_to_tokens_and_ids_stay_local() {
    for spec in REGISTRY {
        for attr in spec.attrs {
            match attr.kind {
                AttrKind::Token => assert!(
                    !attr.vocabulary.is_empty(),
                    "{}.{} is a token with no vocabulary",
                    spec.name,
                    attr.key
                ),
                _ => assert!(
                    attr.vocabulary.is_empty(),
                    "{}.{} is not a token but declares a vocabulary",
                    spec.name,
                    attr.key
                ),
            }
            for token in attr.vocabulary {
                assert!(
                    token
                        .chars()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_'),
                    "{}.{} vocabulary token `{}` is not a lowercase name token",
                    spec.name,
                    attr.key,
                    token
                );
            }
            if attr.kind == AttrKind::Id {
                assert_eq!(
                    attr.class,
                    FieldClass::LocalReference,
                    "{}.{} is an id and must be local-reference — ids never export",
                    spec.name,
                    attr.key
                );
            }
            // `ExactCategorical` means "exact value, cardinality reviewed
            // where it is declared" — a token, a flag, a version, a signed
            // quantity or a count. `BucketedNumeric` means what it says, so
            // it and `Bucket` imply each other: a class promising bucketing
            // over an unbucketed value would be the label lying.
            if attr.kind == AttrKind::Token {
                assert_eq!(
                    attr.class,
                    FieldClass::ExactCategorical,
                    "{}.{} is a token, so its class is exact-categorical",
                    spec.name,
                    attr.key
                );
            }
            assert_eq!(
                attr.kind == AttrKind::Bucket,
                attr.class == FieldClass::BucketedNumeric,
                "{}.{}: bucketed-numeric and the bucket kind imply each other",
                spec.name,
                attr.key
            );
        }
    }
}

/// The Store's catalog facts, as one block: every row a window's profile and
/// workspace menus invalidate on. They carry **ids and closed tokens only** —
/// the registry cannot express a name, a path or a colour, and this test says
/// so out loud so a later row cannot quietly become the second copy of the
/// catalog that R3 exists to avoid.
#[test]
fn the_store_catalog_rows_carry_ids_and_closed_tokens_only() {
    const CATALOG: &[&str] = &[
        "profile.created",
        "profile.renamed",
        "profile.appearance_changed",
        "profile.removal_changed",
        "profile.deleted",
        "profile.restored",
        "workspace.catalog.created",
        "workspace.catalog.renamed",
        "workspace.catalog.appearance_changed",
        "workspace.catalog.archived",
        "workspace.catalog.unarchived",
        "workspace.catalog.deleted",
        "workspace.catalog.restored",
        "workspace.catalog.moved",
        "workspace.catalog.imported",
        "workspace.catalog.reordered",
    ];
    for name in CATALOG {
        let spec = registry::spec(name).unwrap_or_else(|| panic!("{name} is not declared"));
        assert_eq!(
            spec.class,
            EventClass::Domain,
            "{name} is a fact about the workspace, not an app observation"
        );
        assert_eq!(
            spec.telemetry,
            TelemetryPolicy::Never,
            "{name} is local invalidation traffic; exporting it is a privacy-class review"
        );
        for attr in spec.attrs {
            match attr.kind {
                AttrKind::Id => assert_eq!(attr.class, FieldClass::LocalReference),
                AttrKind::Token => assert!(!attr.vocabulary.is_empty()),
                other => panic!(
                    "{name}.{} is a {:?} — a catalog fact carries an id or a closed token and nothing else",
                    attr.key, other
                ),
            }
        }
    }
    // And the whole block is reachable by the two prefixes the client's
    // `CatalogWatch` classifies on. A row that matched neither would be
    // published and never invalidate anything.
    for name in CATALOG {
        assert!(
            name.starts_with("profile.") || name.starts_with("workspace."),
            "{name} is outside the prefixes koru_api_client::events classifies"
        );
    }
}

/// The three workspace-creation rows answer three different questions, and the
/// registry says which is which. If somebody ever collapses them, this is the
/// test that asks them to write down why first.
#[test]
fn workspace_creation_has_three_rows_on_purpose() {
    for (name, must_say) in [
        ("workspace.created", "the app"),
        ("workspace.observed", "commit feed"),
        ("workspace.catalog.created", "every writer"),
    ] {
        let spec = registry::spec(name).unwrap();
        assert!(
            spec.doc.contains(must_say),
            "{name}'s doc no longer says who emits it ({must_say})"
        );
    }
    // Only the app's row carries a source: the Store cannot tell onboarding
    // from manual and must not guess one.
    assert!(
        registry::spec("workspace.created")
            .unwrap()
            .attr("source")
            .is_some()
    );
    assert!(
        registry::spec("workspace.catalog.created")
            .unwrap()
            .attrs
            .is_empty()
    );
}

// --------------------------------------------------------------- the id newtypes

/// An id survives a round trip through the string form the sink port carries.
#[test]
fn an_id_parses_back_from_its_display_form() {
    let event_id = EventId::mint();
    let parsed: EventId = event_id.to_string().parse().expect("its own string form");
    assert_eq!(parsed, event_id);
    let session_id = SessionId::mint();
    let parsed: SessionId = session_id.to_string().parse().expect("its own string form");
    assert_eq!(parsed, session_id);
}

/// A string that is not a uuid is refused, not wrapped: the upload client
/// parses ids the drain hands it, and a silent re-mint would fork the dedupe
/// key the whole pipeline shares.
#[test]
fn a_malformed_id_is_refused() {
    assert!("not-a-uuid".parse::<EventId>().is_err());
    assert!("".parse::<SessionId>().is_err());
}

// ------------------------------------------------------------ task activity

/// ADR-273's table, held to verbatim. The names are listed here rather than
/// derived from the registry so that a row quietly renamed, dropped or added
/// fails against the decision instead of agreeing with itself.
const TASK_EVENTS: &[&str] = &[
    "task.created",
    "task.removed",
    "task.completed",
    "task.reopened",
    "task.due.changed",
    "task.priority.changed",
    "task.child.added",
    "task.child.moved",
    "task.child.removed",
    "task.child.completed",
    "task.child.reopened",
    "task.content.changed",
];

#[test]
fn the_twelve_task_events_are_registered_and_are_the_only_ones() {
    for name in TASK_EVENTS {
        assert!(
            registry::is_registered(name),
            "{name} is in ADR-273's table and not in the registry"
        );
    }
    let registered: Vec<&str> = REGISTRY
        .iter()
        .map(|spec| spec.name)
        .filter(|name| name.starts_with("task."))
        .collect();
    assert_eq!(
        registered, TASK_EVENTS,
        "the `task` area holds ADR-273's twelve, in its order, and nothing else"
    );
}

/// SPEC §9's 90-day domain retention, and `Never` rather than
/// anonymous-usage: task activity is a private work log, and an exportable
/// policy needs a privacy-class review this milestone is not the place for.
/// Reversing it is a review, not an edit — so it is asserted rather than
/// trusted.
#[test]
fn task_activity_is_domain_and_never_leaves_the_device() {
    for name in TASK_EVENTS {
        let spec = registry::spec(name).expect("registered above");
        assert_eq!(spec.class, EventClass::Domain, "{name}");
        assert_eq!(spec.telemetry, TelemetryPolicy::Never, "{name}");
        assert_eq!(spec.version, 1, "{name}");
    }
}

/// The structural half of "payloads carry references and stable tokens only":
/// every attr on every task event is a closed-vocabulary token, a flag, or a
/// local reference. No count, no time, no version — and `AttrValue` has no
/// free string at all, which is what makes a due *date* unrepresentable here.
#[test]
fn no_task_attr_can_carry_a_value() {
    for name in TASK_EVENTS {
        let spec = registry::spec(name).expect("registered above");
        for attr in spec.attrs {
            match attr.kind {
                AttrKind::Token => assert!(!attr.vocabulary.is_empty(), "{name}.{}", attr.key),
                AttrKind::Flag => {}
                AttrKind::Id => assert_eq!(attr.class, FieldClass::LocalReference),
                other => panic!(
                    "{name}.{} is a {} — task activity carries references and closed tokens only",
                    attr.key,
                    other.as_str()
                ),
            }
        }
    }
}

fn a_task() -> TaskSubject {
    TaskSubject {
        workspace: Uuid::from_u128(0x5555_5555_5555_5555_5555_5555_5555_5555),
        document: Uuid::from_u128(0xaaaa_aaaa_aaaa_aaaa_aaaa_aaaa_aaaa_aaaa),
        node: Uuid::from_u128(0xbbbb_bbbb_bbbb_bbbb_bbbb_bbbb_bbbb_bbbb),
    }
}

/// One draft per registered task event, in the table's order — the list the
/// payload and vector tests both walk.
fn every_task_payload() -> Vec<(&'static str, Draft)> {
    let task = a_task();
    let child = Uuid::from_u128(0xcccc_cccc_cccc_cccc_cccc_cccc_cccc_cccc);
    vec![
        (
            "task.created",
            TaskCreated {
                task,
                reason: TaskCreatedReason::Due,
                node_kind: TaskNodeKind::Paragraph,
            }
            .into(),
        ),
        ("task.removed", TaskRemoved { task }.into()),
        (
            "task.completed",
            TaskCompleted {
                task,
                reason: TaskCompletedReason::CascadeFromAncestor,
            }
            .into(),
        ),
        (
            "task.reopened",
            TaskReopened {
                task,
                reason: TaskReopenedReason::UnfinishedChildAdded,
            }
            .into(),
        ),
        (
            "task.due.changed",
            TaskDueChanged {
                task,
                change: TaskAttributeChange::Changed,
                temporal_kind: TaskTemporalKind::DatetimeZoned,
            }
            .into(),
        ),
        (
            "task.priority.changed",
            TaskPriorityChanged {
                task,
                change: TaskAttributeChange::Set,
                priority: Some(TaskPriority::Urgent),
            }
            .into(),
        ),
        (
            "task.child.added",
            TaskChildAdded {
                task,
                child,
                child_class: TaskChildClass::UnfinishedStructural,
                reopened: true,
            }
            .into(),
        ),
        (
            "task.child.moved",
            TaskChildMoved {
                task,
                child,
                child_class: TaskChildClass::Passive,
                reopened: false,
            }
            .into(),
        ),
        (
            "task.child.removed",
            TaskChildRemoved { task, child }.into(),
        ),
        (
            "task.child.completed",
            TaskChildCompleted {
                task,
                child,
                child_is_task: true,
            }
            .into(),
        ),
        (
            "task.child.reopened",
            TaskChildReopened {
                task,
                child,
                child_is_task: false,
                reopened_parent: true,
            }
            .into(),
        ),
        ("task.content.changed", TaskContentChanged { task }.into()),
    ]
}

#[test]
fn every_task_payload_carries_the_task_subject_and_its_workspace() {
    let ctx = fixed_context();
    let names: Vec<&str> = every_task_payload().iter().map(|(name, _)| *name).collect();
    assert_eq!(names, TASK_EVENTS, "one payload per row, in the row order");
    for (name, draft) in every_task_payload() {
        let envelope = draft.build(&ctx, fixed_event_id(), FIXED_TIME);
        validate(&envelope)
            .unwrap_or_else(|violation| panic!("{name} fails its registry row: {violation}"));
        let subject = envelope.subject.as_ref().expect("a task event names one");
        assert_eq!(subject.kind, SubjectKind::Task, "{name}");
        assert_eq!(
            subject.id,
            format!(
                "task:{}:{}:{}",
                a_task().workspace,
                a_task().document,
                a_task().node
            ),
            "{name} spells its subject `task:<workspace>:<document>:<node>`"
        );
        assert_eq!(
            envelope.scope.workspace_id,
            Some(WorkspaceId(a_task().workspace)),
            "{name} sets the workspace scope"
        );
    }
}

/// Absence means removed, never `low` — the one optional attr in the twelve.
#[test]
fn a_cleared_priority_carries_no_priority() {
    let ctx = fixed_context();
    let envelope = Draft::from(TaskPriorityChanged {
        task: a_task(),
        change: TaskAttributeChange::Cleared,
        priority: None,
    })
    .build(&ctx, fixed_event_id(), FIXED_TIME);
    validate(&envelope).unwrap();
    assert!(!envelope.attrs.contains_key("priority"));
}

/// A command's events are a **set** sharing one chain root, not a causal
/// chain among themselves: the command caused all of them, none of them
/// caused another, so `causation_id` stays empty and keeps its documented
/// meaning. Grouping a cascade is then one `WHERE` on the correlation.
#[test]
fn one_commands_events_share_a_root_and_name_no_cause() {
    let ctx = fixed_context();
    let causation = EventId(Uuid::from_u128(0xdddd_dddd_dddd_dddd_dddd_dddd_dddd_dddd));
    let cascaded: Vec<Envelope> = every_task_payload()
        .into_iter()
        .enumerate()
        .map(|(index, (_, draft))| {
            draft.in_chain(causation).build(
                &ctx,
                EventId(Uuid::from_u128(0xe000 + index as u128)),
                FIXED_TIME,
            )
        })
        .collect();
    for envelope in &cascaded {
        validate(envelope).unwrap();
        assert_eq!(envelope.correlation_id, Some(causation));
        assert_eq!(
            envelope.causation_id, None,
            "{} named a cause; the command is the cause, and it is not an event",
            envelope.name
        );
        assert_eq!(envelope.chain_root(), causation);
    }
}

/// `task.content.changed` belongs to no command: it is its own root, and a
/// reader grouping by `chain_root` sees exactly one event.
#[test]
fn a_content_burst_is_its_own_root() {
    let ctx = fixed_context();
    let envelope = Draft::from(TaskContentChanged { task: a_task() }).build(
        &ctx,
        fixed_event_id(),
        FIXED_TIME,
    );
    assert_eq!(envelope.correlation_id, None);
    assert_eq!(envelope.causation_id, None);
    assert_eq!(envelope.chain_root(), envelope.event_id);
    assert!(
        envelope.attrs.is_empty(),
        "the coalesced event carries nothing — never the text"
    );
}

// ------------------------------------------------------- building and rules

fn fixed_context() -> EmitContext {
    EmitContext {
        session_id: SessionId(Uuid::from_u128(0x1111_1111_1111_1111_1111_1111_1111_1111)),
        device_id: Some(DeviceId(Uuid::from_u128(
            0x2222_2222_2222_2222_2222_2222_2222_2222,
        ))),
        profile_id: Some(ProfileId(Uuid::from_u128(
            0x3333_3333_3333_3333_3333_3333_3333_3333,
        ))),
        actor: Actor::user(),
    }
}

const FIXED_TIME: i64 = 1_756_000_000_000;

fn fixed_event_id() -> EventId {
    EventId(Uuid::from_u128(0x4444_4444_4444_4444_4444_4444_4444_4444))
}

#[test]
fn a_typed_payload_builds_a_valid_envelope() {
    let ctx = fixed_context();
    let payloads: Vec<Draft> = vec![
        WorkspaceCreated {
            workspace_id: Uuid::from_u128(0x5555_5555_5555_5555_5555_5555_5555_5555),
            source: WorkspaceSource::Manual,
        }
        .into(),
        AppSessionStarted {
            previous_session_clean: PreviousSession::Clean,
        }
        .into(),
        AppSessionEnded {
            duration_bucket: bucket(90),
        }
        .into(),
        SearchOpened {
            surface: SearchSurface::Overlay,
        }
        .into(),
        ImportCompleted {
            source: ImportSource::Onenote,
            document_count_bucket: bucket(37),
            outcome: ImportOutcome::Ok,
        }
        .into(),
        VoiceDictationCompleted {
            outcome: DictationOutcome::Committed,
            surface: DictationSurface::Document,
            language: "pt_br",
            engine: VoiceEngine::Sherpa,
            duration_bucket: bucket(12),
        }
        .into(),
        VoiceModelInstalled {
            tier: VoiceTier::HighAccuracy,
            engine: VoiceEngine::Whisper,
            version: "1.2.0".into(),
            size_mb_bucket: bucket(648),
        }
        .into(),
        AccountOperationCompleted {
            client: AccountClient::Native,
            provider: AccountProvider::Google,
            platform: AccountPlatform::Windows,
            stage: AccountStage::AuthCallback,
            outcome: AccountOutcome::Succeeded,
            failure: AccountFailure::None,
            duration_ms_bucket: bucket(73),
        }
        .into(),
        TerminalSurfaceOpened {
            surface: TerminalSurface::Terminal,
        }
        .into(),
        TerminalSessionStarted {
            mode: TerminalMode::Managed,
            profile_kind: TerminalProfileKind::Agent,
            provider_kind: TerminalProviderKind::Claude,
        }
        .into(),
        TerminalSessionReattached.into(),
        TerminalSessionEnded {
            outcome: TerminalSessionOutcome::Exited,
            duration_minutes_bucket: bucket(90),
        }
        .into(),
        TerminalInputSubmitted {
            input_kind: TerminalInputKind::Mixed,
        }
        .into(),
        TerminalAttachmentDelivered {
            delivery_mode: TerminalDeliveryMode::Native,
            outcome: TerminalDeliveryOutcome::Delivered,
        }
        .into(),
        TerminalExperienceSummary {
            first_present_ms_bucket: Some(bucket(73)),
            delta_present_p95_ms_bucket: Some(bucket(19)),
            sample_count_bucket: Some(bucket(250)),
            ..Default::default()
        }
        .into(),
    ];
    for draft in payloads {
        let envelope = draft.build(&ctx, fixed_event_id(), FIXED_TIME);
        validate(&envelope).unwrap_or_else(|violation| {
            panic!("{} fails its own registry row: {violation}", envelope.name)
        });
    }
}

#[test]
fn the_boundary_refuses_what_the_registry_does_not_declare() {
    let ctx = fixed_context();
    let build = |draft: Draft| draft.build(&ctx, fixed_event_id(), FIXED_TIME);

    let unknown = build(Draft::new(EventName::new("document.sneezed")));
    assert_eq!(validate(&unknown).unwrap_err().code(), "unknown_event");

    let malformed = build(Draft::new(EventName::new("Kitchen.Sink!")));
    assert_eq!(validate(&malformed).unwrap_err().code(), "malformed_name");

    let undeclared =
        build(Draft::new(event_name!("document.created")).attr("sneaky", AttrValue::Count(1)));
    assert_eq!(validate(&undeclared).unwrap_err().code(), "undeclared_attr");

    let missing = build(Draft::new(event_name!("workspace.created")));
    assert_eq!(validate(&missing).unwrap_err().code(), "missing_attr");

    let outside = build(
        Draft::new(event_name!("workspace.created"))
            .attr("source", AttrValue::Token("acquisition_of_acme".into())),
    );
    assert_eq!(
        validate(&outside).unwrap_err().code(),
        "token_outside_vocabulary"
    );

    let wrong_kind =
        build(Draft::new(event_name!("workspace.created")).attr("source", AttrValue::Count(3)));
    assert_eq!(validate(&wrong_kind).unwrap_err().code(), "wrong_attr_kind");

    let terminal_unknown = build(
        Draft::new(event_name!("terminal.session.started"))
            .attr("mode", AttrValue::Token("raw".into()))
            .attr("profile_kind", AttrValue::Token("shell".into()))
            .attr(
                "provider_kind",
                AttrValue::Token("secret-provider-id".into()),
            ),
    );
    assert_eq!(
        validate(&terminal_unknown).unwrap_err().code(),
        "token_outside_vocabulary"
    );

    let mut stale = build(Draft::new(event_name!("document.created")));
    stale.occurred_at_ms = 0;
    assert_eq!(validate(&stale).unwrap_err().code(), "bad_timestamp");

    let mut wrong_version = build(Draft::new(event_name!("document.created")));
    wrong_version.version = 9;
    assert_eq!(
        validate(&wrong_version).unwrap_err().code(),
        "unknown_version"
    );
}

#[test]
fn a_reaction_joins_its_causes_chain() {
    let ctx = fixed_context();
    let root =
        Draft::new(event_name!("document.created")).build(&ctx, fixed_event_id(), FIXED_TIME);
    assert_eq!(root.chain_root(), root.event_id);

    let reaction_id = EventId(Uuid::from_u128(0x6666_6666_6666_6666_6666_6666_6666_6666));
    let reaction = Draft::new(event_name!("document.updated"))
        .caused_by(&root)
        .build(&ctx, reaction_id, FIXED_TIME + 1);
    assert_eq!(reaction.causation_id, Some(root.event_id));
    assert_eq!(reaction.chain_root(), root.event_id);

    let third_id = EventId(Uuid::from_u128(0x7777_7777_7777_7777_7777_7777_7777_7777));
    let third = Draft::new(event_name!("document.updated"))
        .caused_by(&reaction)
        .build(&ctx, third_id, FIXED_TIME + 2);
    assert_eq!(third.causation_id, Some(reaction.event_id));
    assert_eq!(
        third.chain_root(),
        root.event_id,
        "a chain's root never moves"
    );
}

#[test]
fn the_bucket_ladder_floors() {
    assert_eq!(bucket(0), 0);
    assert_eq!(bucket(1), 1);
    assert_eq!(bucket(4), 3);
    assert_eq!(bucket(37), 25);
    assert_eq!(bucket(100), 100);
    assert_eq!(bucket(999_999), 10_000);
}

// ----------------------------------------------------------- the projection

#[test]
fn nothing_projects_without_consent() {
    let ctx = fixed_context();
    let envelope = Draft::from(SearchOpened {
        surface: SearchSurface::Overlay,
    })
    .build(&ctx, fixed_event_id(), FIXED_TIME);
    assert!(project(&envelope, &Consent::none()).is_none());
}

fn granted() -> Consent {
    Consent {
        granted: true,
        consent_version: "test-1".into(),
    }
}

#[test]
fn a_never_policy_never_projects() {
    let ctx = fixed_context();
    let envelope =
        Draft::new(event_name!("document.trashed")).build(&ctx, fixed_event_id(), FIXED_TIME);
    assert!(project(&envelope, &granted()).is_none());
}

#[test]
fn an_exported_event_carries_no_identity() {
    let ctx = fixed_context();
    let envelope = Draft::from(ImportCompleted {
        source: ImportSource::Markdown,
        document_count_bucket: bucket(3),
        outcome: ImportOutcome::Partial,
    })
    .subject(Subject::document("8888888-not-a-real-doc-id"))
    .build(&ctx, fixed_event_id(), FIXED_TIME);

    let exported = project(&envelope, &granted()).expect("import.completed is exportable");
    assert_eq!(exported.event_id, envelope.event_id);
    assert_eq!(exported.session_id, envelope.origin.session_id);

    let json = serde_json::to_string(&exported.properties).unwrap();
    // No uuid-shaped value and none of the local identity may survive: the
    // projection dropped the subject, the scope, the device and the profile.
    assert!(!json.contains("2222"), "device id leaked: {json}");
    assert!(!json.contains("3333"), "profile id leaked: {json}");
    assert!(
        !json.contains("not-a-real-doc-id"),
        "subject leaked: {json}"
    );
    assert_eq!(exported.properties.len(), 3);
    assert_eq!(
        exported.properties["outcome"],
        serde_json::Value::from("partial")
    );
}

#[test]
fn terminal_telemetry_is_anonymous_bucketed_and_content_free() {
    const SENTINEL: &str = "SECRET prompt C:\\private\\title.txt https://example.invalid";
    let ctx = fixed_context();
    let envelope = Draft::from(TerminalExperienceSummary {
        delta_present_p95_ms_bucket: Some(bucket(37)),
        render_cpu_p95_ms_bucket: Some(bucket(9)),
        stream_gap_count_bucket: Some(bucket(2)),
        sample_count_bucket: Some(bucket(500)),
        ..Default::default()
    })
    .subject(Subject::document(SENTINEL))
    .build(&ctx, fixed_event_id(), FIXED_TIME);

    assert!(project(&envelope, &Consent::none()).is_none());
    let exported = project(&envelope, &granted()).expect("anonymous usage event");
    let json = serde_json::to_string(&exported).unwrap();
    assert!(!json.contains(SENTINEL));
    assert!(!json.contains("2222"), "device identity leaked: {json}");
    assert!(!json.contains("3333"), "profile identity leaked: {json}");
    assert_eq!(exported.properties.len(), 4);
}

#[test]
fn account_telemetry_has_only_reviewed_categories_and_a_bucket() {
    const SENTINEL: &str = "email@example.invalid SECRET-sub SECRET-token SECRET-state SECRET-nonce https://example.invalid/private";
    let ctx = fixed_context();
    let envelope = Draft::from(AccountOperationCompleted {
        client: AccountClient::Web,
        provider: AccountProvider::Google,
        platform: AccountPlatform::Web,
        stage: AccountStage::AuthCallback,
        outcome: AccountOutcome::Failed,
        failure: AccountFailure::ProviderRejected,
        duration_ms_bucket: bucket(73),
    })
    .subject(Subject::document(SENTINEL))
    .build(&ctx, fixed_event_id(), FIXED_TIME);

    let exported = project(&envelope, &granted()).expect("operational account event");
    let json = serde_json::to_string(&exported).unwrap();
    assert!(!json.contains(SENTINEL));
    for forbidden in ["email@example.invalid", "SECRET-", "example.invalid"] {
        assert!(
            !json.contains(forbidden),
            "account telemetry leaked {forbidden}: {json}"
        );
    }
    assert_eq!(exported.properties.len(), 7);
    assert_eq!(
        exported.properties["failure"],
        serde_json::Value::from("provider_rejected")
    );
}

/// Cloud MCP's hosted rows (C18): pseudonymous instrumentation, closed tokens
/// and buckets only, and a client's string becomes `other` before it is an
/// attr. Each projects, and the ingest accepts it only in a batch that carries
/// a distinct id — the connection's keyed hash.
#[test]
fn cloud_mcp_telemetry_is_pseudonymous_closed_and_bucketed() {
    use crate::telemetry::{TelemetryBatch, validate_event};
    const SENTINEL: &str = "Acquisition of Acme Corp 11111111-2222-4333-8444-555555555555";

    let rows: Vec<&EventSpec> = REGISTRY
        .iter()
        .filter(|spec| spec.name.starts_with("mcp."))
        .collect();
    assert_eq!(
        rows.iter().map(|spec| spec.name).collect::<Vec<_>>(),
        [
            "mcp.tool.called",
            "mcp.replica.loaded",
            "mcp.replica.torn_down",
            "mcp.save.unknown"
        ]
    );
    for spec in &rows {
        assert_eq!(spec.class, EventClass::Instrumentation, "{}", spec.name);
        assert_eq!(
            spec.telemetry,
            TelemetryPolicy::PseudonymousUsage,
            "{}",
            spec.name
        );
        for attr in spec.attrs {
            assert!(
                matches!(attr.kind, AttrKind::Token | AttrKind::Bucket),
                "{}.{} is neither a closed token nor a bucket",
                spec.name,
                attr.key
            );
            assert!(attr.required && attr.class.exportable());
        }
    }
    // The two names a client or the executor hands the host end in `other`.
    for vocabulary in [registry::MCP_TOOL_TOKENS, registry::MCP_TOOL_CODES] {
        assert_eq!(vocabulary.last(), Some(&"other"));
    }

    let ctx = fixed_context();
    let batch = |identified: bool| TelemetryBatch {
        batch_id: "00000000-0000-4000-8000-0000000000aa".into(),
        sent_at_ms: FIXED_TIME,
        app_version: "0.0.3".into(),
        build_channel: "hosted".into(),
        platform: "linux".into(),
        consent_version: "hosted".into(),
        anonymous_installation_id: identified.then(|| "0123456789abcdef0123456789abcdef".into()),
        events: Vec::new(),
    };
    let drafts: Vec<Draft> = vec![
        McpToolCalled::new("read_document", None, 73, 5 * 1024 + 7).into(),
        McpToolCalled::new(SENTINEL, Some(SENTINEL), 12_000, 0).into(),
        McpToolCalled::new("edit_document", Some("document_claimed"), 40, 0).into(),
        McpReplicaLoaded {
            stage: McpReplicaStage::Full,
            duration_ms_bucket: bucket(1_800),
            documents_bucket: bucket(37),
        }
        .into(),
        McpReplicaTornDown {
            reason: McpTeardownReason::Idle,
            lifetime_s_bucket: bucket(1_800),
        }
        .into(),
        McpSaveUnknown.into(),
    ];
    let mut exported = Vec::new();
    for draft in drafts {
        let envelope =
            draft
                .subject(Subject::document(SENTINEL))
                .build(&ctx, fixed_event_id(), FIXED_TIME);
        validate(&envelope).unwrap();
        let event = project(&envelope, &granted()).expect("pseudonymous usage projects");
        let json = serde_json::to_string(&event).unwrap();
        assert!(!json.contains("Acme") && !json.contains("5555"), "{json}");
        assert_eq!(validate_event(&event, &batch(true)), Ok(()), "{json}");
        assert_eq!(
            validate_event(&event, &batch(false)),
            Err("consent_missing"),
            "no distinct id, no pseudonymous event"
        );
        exported.push(event.properties);
    }
    let token = |value: &str| serde_json::Value::from(value);
    assert_eq!(exported[0]["tool"], token("read_document"));
    assert_eq!(exported[0]["outcome"], token("ok"));
    assert_eq!(exported[0]["code"], token("none"));
    assert_eq!(exported[0]["duration_ms_bucket"], 50);
    assert_eq!(exported[0]["size_kib_bucket"], 5);
    // A client's name for a tool, or a code nobody listed, is `other`.
    assert_eq!(exported[1]["tool"], token("other"));
    assert_eq!(exported[1]["outcome"], token("refused"));
    assert_eq!(exported[1]["code"], token("other"));
    assert_eq!(exported[1]["duration_ms_bucket"], 10_000);
    assert_eq!(exported[2]["code"], token("document_claimed"));
    assert_eq!(exported[3]["stage"], token("full"));
    assert_eq!(exported[3]["documents_bucket"], 25);
    assert_eq!(exported[4]["reason"], token("idle"));
    assert_eq!(exported[4]["lifetime_s_bucket"], 1_000);
    assert!(exported[5].is_empty());
    // `none` is the answer's code, never a refusal's.
    assert_eq!(
        McpToolCalled::new("list_tasks", Some("none"), 1, 0),
        McpToolCalled::new("list_tasks", Some("anything"), 1, 0)
    );
}

#[test]
fn the_project_rows_can_name_a_shape_but_never_a_project() {
    // A project's name and folder are the two things that must never reach
    // this bus, and the guard is structural: the rows declare only closed
    // token vocabularies, so there is no attribute either could be put in.
    for (name, declared) in [
        (
            "terminal.project.created",
            vec![
                ("scope", vec!["workspace", "unscoped"]),
                ("root", vec!["set", "unset"]),
            ],
        ),
        (
            "terminal.project.deleted",
            vec![("scope", vec!["workspace", "unscoped"])],
        ),
    ] {
        let spec = crate::registry::spec(name).expect("registered");
        let actual: Vec<_> = spec
            .attrs
            .iter()
            .map(|attr| {
                // Every attribute is a closed token, so `root` can only ever
                // say whether a folder was chosen — never which one.
                assert_eq!(attr.kind, AttrKind::Token, "{name}.{}", attr.key);
                (attr.key, attr.vocabulary.to_vec())
            })
            .collect();
        assert_eq!(actual, declared, "{name} grew an attribute");
    }

    let ctx = fixed_context();
    let created = Draft::from(TerminalProjectCreated {
        scope: TerminalProjectScope::Workspace,
        root: TerminalProjectRoot::Set,
    })
    .build(&ctx, fixed_event_id(), FIXED_TIME);
    validate(&created).unwrap();
    // No subject and no workspace scope: a project id resolves nowhere off
    // this device, and the workspace it names is not this event's business.
    assert!(created.subject.is_none());
    assert!(created.scope.workspace_id.is_none());

    let exported = project(&created, &granted()).expect("anonymous usage event");
    let json = serde_json::to_string(&exported).unwrap();
    assert!(!json.contains("2222"), "device identity leaked: {json}");
    assert!(!json.contains("3333"), "profile identity leaked: {json}");
    assert_eq!(exported.properties.len(), 2);
    assert_eq!(
        exported.properties["scope"],
        serde_json::Value::from("workspace")
    );
}

// ------------------------------------------------- generated docs + vectors

/// `reference/EVENTS.md` is generated, never hand-edited — the koru-env mechanism.
#[test]
fn the_generated_events_table_is_up_to_date() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../reference/EVENTS.md");
    let Some(parent) = path.parent() else { return };
    if !parent.is_dir() {
        return; // packaged build
    }
    let generated = docs::markdown();
    if update_requested() {
        std::fs::write(&path, &generated).expect("write reference/EVENTS.md");
        return;
    }
    let current = std::fs::read_to_string(&path).unwrap_or_default();
    assert_eq!(
        current, generated,
        "reference/EVENTS.md is stale; regenerate with `KORU_UPDATE_EVENTS=1 cargo test -p koru-events`"
    );
}

/// The serialization vectors are the cross-team drift defense: the metrics
/// ingest service and the flow engine diff their decoders against these
/// files, so a silent reinterpretation of a field fails here first.
#[test]
fn the_serialization_vectors_are_current() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/vectors");
    let ctx = fixed_context();
    let mut vectors: Vec<(&str, Envelope)> = vec![
        (
            "workspace.created",
            Draft::from(WorkspaceCreated {
                workspace_id: Uuid::from_u128(0x5555_5555_5555_5555_5555_5555_5555_5555),
                source: WorkspaceSource::Onboarding,
            })
            .build(&ctx, fixed_event_id(), FIXED_TIME),
        ),
        (
            "profile.created",
            Draft::from(ProfileCreated {
                profile_id: Uuid::from_u128(0x7777_7777_7777_7777_7777_7777_7777_7777),
            })
            .build(&ctx, fixed_event_id(), FIXED_TIME),
        ),
        (
            "workspace.catalog.created",
            Draft::from(WorkspaceCatalogCreated {
                workspace_id: Uuid::from_u128(0x5555_5555_5555_5555_5555_5555_5555_5555),
            })
            .build(&ctx, fixed_event_id(), FIXED_TIME),
        ),
        (
            "workspace.catalog.archived",
            Draft::from(WorkspaceCatalogArchived {
                workspace_id: Uuid::from_u128(0x5555_5555_5555_5555_5555_5555_5555_5555),
            })
            .build(&ctx, fixed_event_id(), FIXED_TIME),
        ),
        (
            "workspace.catalog.imported",
            Draft::from(WorkspaceCatalogImported {
                workspace_id: Uuid::from_u128(0x5555_5555_5555_5555_5555_5555_5555_5555),
                mode: WorkspaceImportMode::Restore,
            })
            .build(&ctx, fixed_event_id(), FIXED_TIME),
        ),
        (
            "workspace.catalog.reordered",
            Draft::from(WorkspaceCatalogReordered {
                profile_id: Uuid::from_u128(0x7777_7777_7777_7777_7777_7777_7777_7777),
            })
            .build(&ctx, fixed_event_id(), FIXED_TIME),
        ),
        // The one catalog row with two reference attrs. Pinned as a vector so
        // the pair cannot be collapsed to a single "profile" later: a reader
        // that lost `from_profile` could no longer tell a move from a create.
        (
            "workspace.catalog.moved",
            Draft::from(WorkspaceCatalogMoved {
                workspace_id: Uuid::from_u128(0x5555_5555_5555_5555_5555_5555_5555_5555),
                from_profile: Uuid::from_u128(0x7777_7777_7777_7777_7777_7777_7777_7777),
                to_profile: Uuid::from_u128(0x8888_8888_8888_8888_8888_8888_8888_8888),
            })
            .build(&ctx, fixed_event_id(), FIXED_TIME),
        ),
        (
            "app.session.started",
            Draft::from(AppSessionStarted {
                previous_session_clean: PreviousSession::Unclean,
            })
            .build(&ctx, fixed_event_id(), FIXED_TIME),
        ),
        (
            "app.session.ended",
            Draft::from(AppSessionEnded {
                duration_bucket: bucket(90),
            })
            .build(&ctx, fixed_event_id(), FIXED_TIME),
        ),
        (
            "search.opened",
            Draft::from(SearchOpened {
                surface: SearchSurface::Window,
            })
            .build(&ctx, fixed_event_id(), FIXED_TIME),
        ),
        (
            "import.completed",
            Draft::from(ImportCompleted {
                source: ImportSource::LegacyWorkspace,
                document_count_bucket: bucket(37),
                outcome: ImportOutcome::Failed,
            })
            .build(&ctx, fixed_event_id(), FIXED_TIME),
        ),
        (
            "document.created",
            Draft::new(event_name!("document.created"))
                .actor(Actor::system("store"))
                .subject(Subject::document(
                    Uuid::from_u128(0x9999_9999_9999_9999_9999_9999_9999_9999).to_string(),
                ))
                .workspace(WorkspaceId(Uuid::from_u128(
                    0x5555_5555_5555_5555_5555_5555_5555_5555,
                )))
                .build(&ctx, fixed_event_id(), FIXED_TIME),
        ),
        (
            "account.operation.completed",
            Draft::from(AccountOperationCompleted {
                client: AccountClient::Native,
                provider: AccountProvider::Google,
                platform: AccountPlatform::Macos,
                stage: AccountStage::SessionExchange,
                outcome: AccountOutcome::Succeeded,
                failure: AccountFailure::None,
                duration_ms_bucket: bucket(73),
            })
            .build(&ctx, fixed_event_id(), FIXED_TIME),
        ),
        (
            "terminal.surface.opened",
            Draft::from(TerminalSurfaceOpened {
                surface: TerminalSurface::Terminal,
            })
            .build(&ctx, fixed_event_id(), FIXED_TIME),
        ),
        (
            "terminal.session.started",
            Draft::from(TerminalSessionStarted {
                mode: TerminalMode::Managed,
                profile_kind: TerminalProfileKind::Agent,
                provider_kind: TerminalProviderKind::Codex,
            })
            .build(&ctx, fixed_event_id(), FIXED_TIME),
        ),
        (
            "terminal.session.reattached",
            Draft::from(TerminalSessionReattached).build(&ctx, fixed_event_id(), FIXED_TIME),
        ),
        (
            "terminal.session.ended",
            Draft::from(TerminalSessionEnded {
                outcome: TerminalSessionOutcome::Terminated,
                duration_minutes_bucket: bucket(90),
            })
            .build(&ctx, fixed_event_id(), FIXED_TIME),
        ),
        (
            "terminal.input.submitted",
            Draft::from(TerminalInputSubmitted {
                input_kind: TerminalInputKind::Mixed,
            })
            .build(&ctx, fixed_event_id(), FIXED_TIME),
        ),
        (
            "terminal.attachment.delivered",
            Draft::from(TerminalAttachmentDelivered {
                delivery_mode: TerminalDeliveryMode::Path,
                outcome: TerminalDeliveryOutcome::Delivered,
            })
            .build(&ctx, fixed_event_id(), FIXED_TIME),
        ),
        (
            "terminal.experience.summary",
            Draft::from(TerminalExperienceSummary {
                first_present_ms_bucket: Some(bucket(73)),
                switch_present_ms_bucket: Some(bucket(19)),
                delta_present_p95_ms_bucket: Some(bucket(37)),
                render_cpu_p95_ms_bucket: Some(bucket(9)),
                jank_per_thousand_bucket: Some(bucket(25)),
                stream_gap_count_bucket: Some(bucket(2)),
                sample_count_bucket: Some(bucket(500)),
            })
            .build(&ctx, fixed_event_id(), FIXED_TIME),
        ),
        (
            "terminal.project.created",
            Draft::from(TerminalProjectCreated {
                scope: TerminalProjectScope::Workspace,
                root: TerminalProjectRoot::Set,
            })
            .build(&ctx, fixed_event_id(), FIXED_TIME),
        ),
        (
            "terminal.project.deleted",
            Draft::from(TerminalProjectDeleted {
                scope: TerminalProjectScope::Unscoped,
            })
            .build(&ctx, fixed_event_id(), FIXED_TIME),
        ),
        (
            "settings.global_changed",
            Draft::from(crate::SettingsGlobalChanged).build(&ctx, fixed_event_id(), FIXED_TIME),
        ),
        (
            "settings.ai_changed",
            Draft::from(crate::SettingsAiChanged).build(&ctx, fixed_event_id(), FIXED_TIME),
        ),
        (
            "settings.terminal_changed",
            Draft::from(crate::SettingsTerminalChanged).build(&ctx, fixed_event_id(), FIXED_TIME),
        ),
    ];
    // The twelve task events, each as a wrapper would send it: one shared
    // correlation for the command's set, and `task.content.changed` — which
    // belongs to no command — as its own root.
    let causation = EventId(Uuid::from_u128(0xdddd_dddd_dddd_dddd_dddd_dddd_dddd_dddd));
    for (name, draft) in every_task_payload() {
        let draft = match name {
            "task.content.changed" => draft,
            _ => draft.in_chain(causation),
        };
        vectors.push((name, draft.build(&ctx, fixed_event_id(), FIXED_TIME)));
    }
    if update_requested() {
        std::fs::create_dir_all(&dir).expect("create tests/vectors");
    }
    for (name, envelope) in vectors {
        let path = dir.join(format!("{name}.json"));
        let generated = serde_json::to_string_pretty(&envelope).unwrap();
        if update_requested() {
            std::fs::write(&path, format!("{generated}\n")).expect("write vector");
            continue;
        }
        let current = std::fs::read_to_string(&path).unwrap_or_default();
        let current: serde_json::Value = serde_json::from_str(current.trim())
            .unwrap_or_else(|_| panic!("{} is missing or unreadable; regenerate with `KORU_UPDATE_EVENTS=1 cargo test -p koru-events`", path.display()));
        let generated: serde_json::Value = serde_json::from_str(&generated).unwrap();
        assert_eq!(
            current, generated,
            "the wire shape of `{name}` changed; if that is deliberate, bump its version and regenerate with `KORU_UPDATE_EVENTS=1 cargo test -p koru-events`"
        );
        // And the vector round-trips back into the type.
        let decoded: Envelope = serde_json::from_value(generated).unwrap();
        validate(&decoded).unwrap();
    }
}

#[test]
fn the_terminal_telemetry_projection_vector_is_current_and_anonymous() {
    const SENTINEL: &str = "SECRET prompt C:\\private\\title.txt https://example.invalid";
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/vectors");
    let ctx = fixed_context();
    let envelope = Draft::from(TerminalExperienceSummary {
        first_present_ms_bucket: Some(bucket(73)),
        delta_present_p95_ms_bucket: Some(bucket(37)),
        render_cpu_p95_ms_bucket: Some(bucket(9)),
        jank_per_thousand_bucket: Some(bucket(25)),
        stream_gap_count_bucket: Some(bucket(2)),
        sample_count_bucket: Some(bucket(500)),
        ..Default::default()
    })
    .subject(Subject::document(SENTINEL))
    .build(&ctx, fixed_event_id(), FIXED_TIME);
    let telemetry = project(&envelope, &granted()).unwrap();
    let generated = serde_json::to_string_pretty(&telemetry).unwrap();
    assert!(!generated.contains(SENTINEL));
    assert!(!generated.contains("2222"), "device identity leaked");
    assert!(!generated.contains("3333"), "profile identity leaked");

    let path = dir.join("telemetry.terminal.experience.summary.json");
    if update_requested() {
        std::fs::create_dir_all(&dir).expect("create tests/vectors");
        std::fs::write(&path, format!("{generated}\n")).expect("write telemetry vector");
        return;
    }
    let current = std::fs::read_to_string(&path).unwrap_or_else(|_| {
        panic!(
            "{} is missing; regenerate with `KORU_UPDATE_EVENTS=1 cargo test -p koru-events`",
            path.display()
        )
    });
    let current: serde_json::Value = serde_json::from_str(current.trim()).unwrap();
    let generated: serde_json::Value = serde_json::from_str(&generated).unwrap();
    assert_eq!(current, generated);
}

fn update_requested() -> bool {
    match std::env::var("KORU_UPDATE_EVENTS") {
        Ok(value) => {
            let value = value.trim().to_ascii_lowercase();
            !matches!(value.as_str(), "" | "0" | "false" | "no" | "off")
        }
        Err(_) => false,
    }
}

// --------------------------------------------------------- the source sweep

/// Every `event_name!("…")` literal anywhere in the tree resolves to a
/// registry row — the koru-env sweep, for names. Typed payloads are already
/// tied at compile time; this catches the escape hatch.
#[test]
fn every_event_name_literal_in_the_tree_is_registered() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    if !root.join("crates").is_dir() || !root.join("apps").is_dir() {
        return; // packaged build; sources absent
    }
    let mut names = std::collections::BTreeSet::new();
    for dir in ["crates", "apps"] {
        collect_names(&root.join(dir), &mut names);
    }
    for name in names {
        assert!(
            registry::is_registered(&name),
            "event_name!(\"{name}\") appears in the tree but has no REGISTRY row"
        );
    }
}

fn collect_names(dir: &std::path::Path, out: &mut std::collections::BTreeSet<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            // Skip this crate (it declares the names) and build output.
            if path.ends_with("koru-events") || path.ends_with("target") {
                continue;
            }
            collect_names(&path, out);
            continue;
        }
        if path.extension().and_then(|e| e.to_str()) != Some("rs") {
            continue;
        }
        let Ok(content) = std::fs::read_to_string(&path) else {
            continue;
        };
        for (index, _) in content.match_indices("event_name!(") {
            let window = &content[index..(index + 96).min(content.len())];
            let Some(start) = window.find('"') else {
                continue;
            };
            let name: String = window[start + 1..]
                .chars()
                .take_while(|c| *c != '"')
                .collect();
            if !name.is_empty() {
                out.insert(name);
            }
        }
    }
}

// -------------------------------------------------------------- wire basics

#[test]
fn optional_envelope_fields_stay_off_the_wire() {
    let ctx = EmitContext {
        session_id: SessionId(Uuid::from_u128(1)),
        device_id: None,
        profile_id: None,
        actor: Actor::user(),
    };
    let envelope =
        Draft::new(event_name!("document.created")).build(&ctx, fixed_event_id(), FIXED_TIME);
    let json = serde_json::to_string(&envelope).unwrap();
    for absent in [
        "scope",
        "subject",
        "correlation_id",
        "causation_id",
        "attrs",
        "device_id",
    ] {
        assert!(
            !json.contains(absent),
            "`{absent}` serialized while empty: {json}"
        );
    }
    let back: Envelope = serde_json::from_str(&json).unwrap();
    assert_eq!(back, envelope);
}

#[test]
fn attr_values_are_adjacently_tagged() {
    let value = AttrValue::Token("manual".into());
    assert_eq!(
        serde_json::to_string(&value).unwrap(),
        r#"{"t":"token","v":"manual"}"#
    );
    let flag = AttrValue::Flag(true);
    assert_eq!(
        serde_json::to_string(&flag).unwrap(),
        r#"{"t":"flag","v":true}"#
    );
}

#[test]
fn attrs_round_trip_through_the_map() {
    let mut attrs = BTreeMap::new();
    attrs.insert("source".to_string(), AttrValue::Token("import".into()));
    attrs.insert("when".to_string(), AttrValue::Time(FIXED_TIME));
    let json = serde_json::to_string(&attrs).unwrap();
    let back: BTreeMap<String, AttrValue> = serde_json::from_str(&json).unwrap();
    assert_eq!(back, attrs);
}

/// **The URL token and the JSON token are the same string, and they have to
/// stay so.** A subject travels as `kind=` on the per-subject activity read
/// and as `"kind":` inside an envelope, and the two spellings are produced by
/// different code — a hand-written match and serde's `rename_all`. A rename on
/// one side without the other would leave a client asking for a kind the
/// worker has never heard of, answered as a 400 nobody could explain.
#[test]
fn a_subject_kind_token_is_what_serde_writes() {
    for kind in SubjectKind::ALL.iter().copied() {
        let json = serde_json::to_string(&kind).unwrap();
        assert_eq!(json, format!("\"{}\"", kind.token()));
        assert_eq!(SubjectKind::from_token(kind.token()), Some(kind));
    }
    assert_eq!(SubjectKind::from_token("sandwich"), None);
}

#[test]
fn evernote_is_a_registered_import_source_token() {
    assert_eq!(ImportSource::Evernote.token(), "evernote");
    let source = registry::spec("import.completed")
        .unwrap()
        .attrs
        .iter()
        .find(|attr| attr.key == "source")
        .unwrap();
    assert!(source.vocabulary.contains(&"evernote"));
}
