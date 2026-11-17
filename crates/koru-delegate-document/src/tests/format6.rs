// SPDX-License-Identifier: MPL-2.0

//! Cohort `m6_run6_format`: Bot Run format 6 (M6).
//!
//! Format 6 is the first that can hold a parent (`bot.parent`), a pinned
//! result (`native_results[].version_id`) and the `waiting_for_bot` marker and
//! step. The goldens below are authored literals of the stored values, and the
//! older build is a frozen copy of the format 5 types, not this build's.

use super::projection::{applied, bot, bot5, bot6, id, intent, next, rev};
use super::*;
use koru_delegate_core::decisions::*;
use koru_store_core::version::Compatibility;

fn stored(doc: &RunDocument, key: &str) -> serde_json::Value {
    let loro::LoroValue::Map(run) = doc.doc().get_map("run").get_deep_value() else {
        panic!("the run root is a map");
    };
    serde_json::Value::from(run.get(key).cloned().expect("the key is stored"))
}

fn literal(text: &str) -> serde_json::Value {
    serde_json::from_str(text).unwrap()
}

fn parent() -> ChildParent {
    ChildParent {
        run: RunId::from_uuid(v4(0x50)),
        bot: BotId::from_uuid(v4(0x51)),
        chat_id: v4(0x52),
        child_request: ChildRequestId::from_uuid(v4(0x53)),
    }
}

/// A child Run's header: an ordinary interactive Bot Run that names who asked.
fn child_header() -> RunHeader {
    let mut header = bots::bot_header();
    header.status = RunStatus::running();
    let bot = header.bot.as_mut().unwrap();
    bot.owner.user_scope = koru_store_core::UserScopeId::from_uuid(v4(0x60));
    bot.parent = Some(parent());
    header
}

fn pinned(n: u8) -> NativeResultReference {
    NativeResultReference {
        operation_id: v4(n),
        workspace: WorkspaceId::from_uuid(v4(46)),
        document: DocumentId::from_uuid(v4(47)),
        version: vec![4, 5, 6],
        title: "Pinned page".into(),
        confirmed_at_ms: 11,
        kind: NativeResultKind::EditedPage,
        version_id: Some(DocumentVersionId::from_uuid(v4(0x54))),
    }
}

/// The wait's opening Control as the ledger composes it: one field-free
/// `waiting_for_bot` step with an attempt reference.
fn child_waiting(doc: &RunDocument) -> RunProjectionIntent {
    let mut value = intent(doc);
    let header = doc.header().unwrap();
    let RunProjectionTargetV1::Control {
        effects,
        wait_phase,
        reason,
        action,
        ..
    } = &mut value.target
    else {
        unreachable!()
    };
    effects.event = Some(RunEvent::WaitingForBot);
    effects.status = transition_bot(&header.status, &RunEvent::WaitingForBot).unwrap();
    effects.steps = DecisionList::new(vec![TimelineEntry {
        id: id(91),
        kind: StepKind::WaitingForBot,
        refs: TimelineRefs::new([TimelineRef::Attempt {
            attempt: attempt(80),
        }])
        .unwrap(),
        at_ms: 8,
    }])
    .unwrap();
    *wait_phase = BotWaitPhase::HoldRequested;
    *reason = BotWaitReason::Child {};
    *action = DecisionControlAction::ChildWaiting {};
    assert!(value.target.validate().is_ok());
    value.identity.target_digest = value.target.digest().unwrap();
    value
}

/// The Control that ends a resumed wait: the existing `Resume` action.
fn child_resumed(doc: &RunDocument, previous: &RunProjectionIntent) -> RunProjectionIntent {
    let mut value = previous.clone();
    let header = doc.header().unwrap();
    value.identity.transition = id(72);
    value.identity.sequence = rev(2);
    value.target = RunProjectionTargetV1::Control {
        version: DecisionVersionV1,
        predecessor: DecisionProjectionPredecessor {
            observed_created_at_ms: header.created_at_ms,
            timeline_start: doc.timeline_len() as u64,
            projection: Some(DecisionProjectionPrior {
                transition: previous.identity.transition,
                sequence: previous.identity.sequence,
                target_digest: previous.identity.target_digest,
            }),
            status: header.status.clone(),
            failure_reason: header.failure_reason,
        },
        effects: DecisionProjectionEffects {
            event: Some(RunEvent::Resume),
            status: transition_bot(&header.status, &RunEvent::Resume).unwrap(),
            failure_reason: None,
            economics_at_ms: None,
            steps: DecisionList::new(vec![TimelineEntry {
                id: id(92),
                kind: StepKind::Resumed {
                    via: ResumeSeam::RecordedContext,
                },
                refs: TimelineRefs::new([TimelineRef::Attempt {
                    attempt: attempt(80),
                }])
                .unwrap(),
                at_ms: 9,
            }])
            .unwrap(),
            models: DecisionList::default(),
            publications: DecisionList::default(),
        },
        fence: *previous.target.fence(),
        disposition: None,
        wait_phase: BotWaitPhase::ResumeReserved,
        reason: BotWaitReason::Child {},
        action: DecisionControlAction::Resume {
            via: ResumeSeam::RecordedContext,
        },
        retained_bookkeeping: previous.target.resulting_bookkeeping().unwrap(),
        flush: DecisionList::default(),
        delivery: None,
    };
    assert!(value.target.validate().is_ok());
    value.identity.target_digest = value.target.digest().unwrap();
    value
}

fn project(doc: &RunDocument, intent: &RunProjectionIntent) {
    let mut stage = RunProjectionStaging::default();
    let prepared = stage.prepare(doc, intent).unwrap();
    let receipt = applied(&mut stage, doc, &prepared);
    assert_eq!(receipt.identity(), &intent.identity);
}

// ---- creation ---------------------------------------------------------------

#[test]
fn m6_run6_format_a_new_bot_run_is_6_and_an_ordinary_run_stays_3() {
    assert_eq!(RUN_BOT_DOCUMENT_SCHEMA_VERSION, 6);
    assert_eq!(RUN_DOCUMENT_SCHEMA_VERSION, 6);
    for header in [bots::bot_header(), child_header()] {
        let run = RunDocument::create(&header).unwrap();
        assert_eq!(run.schema_version(), Some(6));
        assert_eq!(run.compatibility(), Compatibility::Writable);
        assert_eq!(run.header().unwrap(), header);
        let reopened = RunDocument::from_snapshot(&run.export_snapshot().unwrap()).unwrap();
        assert_eq!(reopened.schema_version(), Some(6));
        assert_eq!(reopened.header().unwrap(), header);
    }
    assert_eq!(bot6(true).schema_version(), Some(6));
    assert_eq!(
        RunDocument::create(&header()).unwrap().schema_version(),
        Some(3)
    );
    // Format 6 never holds an ordinary Run.
    let ordinary = RunDocument::create(&header()).unwrap();
    ordinary
        .doc()
        .get_map("run")
        .insert("schema", 6i64)
        .unwrap();
    ordinary.doc().commit();
    assert!(ordinary.header().is_err());
    assert!(ordinary.append_step(&step(StepKind::Started, 2)).is_err());
}

// ---- goldens ----------------------------------------------------------------

const CHILD_BOT: &str = r#"{"owner":{"user_scope":"00000000-0000-4000-8000-000000000060","device":"00000000-0000-4000-8000-000000000009"},"bot":"00000000-0000-4000-8000-000000000028","submission":"00000000-0000-4000-8000-000000000029","chat_id":"00000000-0000-4000-8000-00000000002a","ai_attempt_id":"00000000-0000-4000-8000-00000000002b","assistant_message_id":"00000000-0000-4000-8000-00000000002c","config_revision":2,"policy_revision":1,"continued_from":null,"parent":{"run":"00000000-0000-4000-8000-000000000050","bot":"00000000-0000-4000-8000-000000000051","chat_id":"00000000-0000-4000-8000-000000000052","child_request":"00000000-0000-4000-8000-000000000053"}}"#;

const PINNED_RESULT: &str = r#"{"operation_id":"00000000-0000-4000-8000-000000000055","workspace":"00000000-0000-4000-8000-00000000002e","document":"00000000-0000-4000-8000-00000000002f","version":[4,5,6],"title":"Pinned page","confirmed_at_ms":11,"kind":"edited_page","version_id":"00000000-0000-4000-8000-000000000054"}"#;

const WAITING_STATUS: &str =
    r#"{"state":"running","attention":["waiting_for_bot"],"control":{"holder":"orchestrator"}}"#;

const RUNNING_STATUS: &str =
    r#"{"state":"running","attention":[],"control":{"holder":"orchestrator"}}"#;

const WAITING_STEP: &str = r#"{"entry":"step","id":"00000000-0000-4000-8000-00000000005b","kind":{"step":"waiting_for_bot"},"refs":[{"ref":"attempt","attempt":"00000000-0000-4000-8000-000000000050"}],"at_ms":8}"#;

#[test]
fn m6_run6_format_golden_child_header() {
    let header = child_header();
    let run = RunDocument::create(&header).unwrap();
    assert_eq!(run.schema_version(), Some(6));
    assert_eq!(stored(&run, "bot"), literal(CHILD_BOT));
    // The literal decodes to the same provenance, and the parent is written
    // once: no later write touches it.
    let decoded: BotProvenance = serde_json::from_str(CHILD_BOT).unwrap();
    assert_eq!(Some(&decoded), header.bot.as_ref());
    assert_eq!(decoded.parent, Some(parent()));
    run.append_step(&step(StepKind::Started, 3)).unwrap();
    run.register_native_result(&pinned(0x55)).unwrap();
    run.set_status(&RunStatus::Completed).unwrap();
    assert_eq!(stored(&run, "bot"), literal(CHILD_BOT));
    // A pinned result is stored with the Store version id, after the entry
    // the fixture already held, which has none and gains no key.
    let results = stored(&run, "native_results");
    assert_eq!(results[1], literal(PINNED_RESULT));
    assert!(results[0].get("version_id").is_none());
    let read = run.header().unwrap();
    assert_eq!(read.native_results[1], pinned(0x55));
    assert_eq!(read.native_results[0].version_id, None);
    // A child is never a continuation: the header is refused at creation.
    let mut invalid = child_header();
    invalid.bot.as_mut().unwrap().continued_from = Some(RunId::from_uuid(v4(0x56)));
    assert!(RunDocument::create(&invalid).is_err());
}

#[test]
fn m6_run6_format_golden_waiting_parent_and_resume() {
    let doc = bot6(false);
    assert_eq!(stored(&doc, "status"), literal(RUNNING_STATUS));
    let waiting = child_waiting(&doc);
    assert_eq!(u8::from(waiting.identity.source_schema), 6);
    project(&doc, &waiting);
    assert_eq!(doc.schema_version(), Some(6));
    assert_eq!(stored(&doc, "status"), literal(WAITING_STATUS));
    assert_eq!(doc.timeline_len(), 1);
    let first_entry = doc.doc().get_list("timeline").get(0).unwrap();
    assert_eq!(
        serde_json::Value::from(first_entry.get_deep_value()),
        literal(WAITING_STEP)
    );
    let header = doc.header().unwrap();
    let RunStatus::Running { attention, .. } = &header.status else {
        panic!("a waiting parent is Running");
    };
    assert!(attention.contains(Attention::WaitingForBot));
    assert_eq!(
        doc.timeline_window(0, 1)[0].step().map(|entry| &entry.kind),
        Some(&StepKind::WaitingForBot)
    );
    // The parent header holds no list of child requests.
    assert!(doc.doc().get_map("run").get("child_requests").is_none());
    // The existing Resume action clears the marker and writes Resumed.
    let resumed = child_resumed(&doc, &waiting);
    project(&doc, &resumed);
    assert_eq!(stored(&doc, "status"), literal(RUNNING_STATUS));
    assert_eq!(doc.timeline_len(), 2);
    assert_eq!(
        doc.timeline_window(0, 1)[0].step().map(|entry| &entry.kind),
        Some(&StepKind::Resumed {
            via: ResumeSeam::RecordedContext
        })
    );
    assert!(doc.compatibility().writable());
    doc.append_step(&step(StepKind::Completed, 12)).unwrap();
}

// ---- earlier formats --------------------------------------------------------

#[test]
fn m6_run6_format_a_5_document_reads_with_the_new_fields_absent_and_stays_5_until_a_projection() {
    for routine in [false, true] {
        let doc = bot5(routine);
        let reopened = RunDocument::from_snapshot(&doc.export_snapshot().unwrap()).unwrap();
        assert_eq!(reopened.schema_version(), Some(5));
        assert_eq!(reopened.compatibility(), Compatibility::Writable);
        let header = reopened.header().unwrap();
        assert_eq!(header.bot.as_ref().unwrap().parent, None);
        assert!(header.native_results.iter().all(|r| r.version_id.is_none()));
        assert!(stored(&reopened, "bot").get("parent").is_none());
        // Every ordinary write leaves it at 5, a save without a pin included.
        reopened.append_step(&step(StepKind::Started, 30)).unwrap();
        reopened.set_status(&RunStatus::running()).unwrap();
        let mut unpinned = pinned(0x57);
        unpinned.version_id = None;
        reopened.register_native_result(&unpinned).unwrap();
        reopened.record_model(&identity(0x30)).unwrap();
        reopened.refresh_economics(40).unwrap();
        assert_eq!(reopened.schema_version(), Some(5));
        // A projection commits 6.
        let first = intent(&reopened);
        assert_eq!(u8::from(first.identity.source_schema), 5);
        project(&reopened, &first);
        assert_eq!(reopened.schema_version(), Some(6));
        assert!(reopened.compatibility().writable());
        assert_eq!(reopened.header().unwrap().bot, header.bot);
    }
}

#[test]
fn m6_run6_format_a_projection_upgrades_3_4_and_5_to_6_and_the_next_one_may_still_name_5() {
    let documents = [
        (bot(false), 3u8),
        (bot(true), 4),
        (bot5(false), 5),
        (bot5(true), 5),
        (bot6(false), 6),
        (bot6(true), 6),
    ];
    for (doc, created_at) in documents {
        let first = intent(&doc);
        assert_eq!(u8::from(first.identity.source_schema), created_at);
        let mut stage = RunProjectionStaging::default();
        let prepared = stage.prepare(&doc, &first).unwrap();
        // Preparing writes nothing: the format moves only with the commit.
        assert_eq!(doc.schema_version(), Some(i64::from(created_at)));
        applied(&mut stage, &doc, &prepared);
        assert_eq!(doc.schema_version(), Some(6));
        // A lost acknowledgement: the same retained preparation applies again
        // on a cold handle, although the document no longer shows the named
        // format.
        RunProjectionStaging::default()
            .apply(&doc, &prepared)
            .unwrap();
        // After an applied projection the ledger names 5 (or 6 when it was
        // told 6). Both are accepted by a document a projection left at 6.
        for named in [5u8, 6] {
            let mut following = next(&doc, &first, RunEvent::Cancel);
            following.identity.source_schema = named.try_into().unwrap();
            let before = doc.version();
            let prepared = RunProjectionStaging::default()
                .prepare(&doc, &following)
                .unwrap();
            assert_eq!(doc.version(), before);
            assert_eq!(u8::from(prepared.identity.source_schema), named);
        }
        let following = next(&doc, &first, RunEvent::Cancel);
        assert_eq!(u8::from(following.identity.source_schema), 5);
        project(&doc, &following);
        assert_eq!(doc.schema_version(), Some(6));
        assert_eq!(doc.header().unwrap().status, RunStatus::Cancelled);
    }
}

#[test]
fn m6_run6_format_a_named_format_never_crosses_a_cohort_or_moves_down() {
    // A document below 6 must show exactly the named format.
    for (doc, named) in [
        (bot5(false), 6u8),
        (bot5(false), 3),
        (bot5(true), 4),
        (bot(false), 5),
        (bot(false), 6),
        (bot(true), 5),
    ] {
        let mut offered = intent(&doc);
        offered.identity.source_schema = named.try_into().unwrap();
        let before = doc.version();
        assert!(
            RunProjectionStaging::default()
                .prepare(&doc, &offered)
                .is_err()
        );
        assert_eq!(doc.version(), before);
    }
    // A document at 6 accepts an earlier name only for the cohort it holds:
    // 3 names a Manual Bot Run, 4 a Routine one.
    for (routine, named, accepted) in [
        (false, 3u8, true),
        (false, 4, false),
        (true, 4, true),
        (true, 3, false),
        (false, 5, true),
        (true, 5, true),
    ] {
        let doc = bot6(routine);
        let mut offered = intent(&doc);
        offered.identity.source_schema = named.try_into().unwrap();
        assert_eq!(
            RunProjectionStaging::default()
                .prepare(&doc, &offered)
                .is_ok(),
            accepted,
            "routine {routine}, named {named}"
        );
    }
}

#[test]
fn m6_run6_format_a_pinned_save_stamps_6_in_its_own_commit() {
    for (doc, created_at) in [(bot(false), 3i64), (bot(true), 4), (bot5(false), 5)] {
        assert_eq!(doc.schema_version(), Some(created_at));
        // The ledger's first projection base was read at the earlier format.
        let first = intent(&doc);
        let reference = pinned(0x58);
        let replica = RunDocument::from_snapshot(&doc.export_snapshot().unwrap()).unwrap();
        let before = doc.version();
        doc.register_native_result(&reference).unwrap();
        assert_eq!(doc.schema_version(), Some(6));
        // One update carries the entry and the format together: a replica
        // never sees the pinned entry under the earlier number.
        replica
            .import(&doc.export_updates_since(&before).unwrap())
            .unwrap();
        assert_eq!(replica.schema_version(), Some(6));
        assert_eq!(
            replica.header().unwrap().native_results.last(),
            Some(&reference)
        );
        // An exact replay writes nothing; a different version id for the same
        // operation is refused and writes nothing.
        let settled = doc.version();
        doc.register_native_result(&reference).unwrap();
        let mut other = reference.clone();
        other.version_id = Some(DocumentVersionId::from_uuid(v4(0x59)));
        assert!(doc.register_native_result(&other).is_err());
        let mut unpinned = reference.clone();
        unpinned.version_id = None;
        assert!(doc.register_native_result(&unpinned).is_err());
        assert_eq!(doc.version(), settled);
        // The projection whose base named the earlier format still prepares
        // and applies.
        assert_eq!(
            i64::from(u8::from(first.identity.source_schema)),
            created_at
        );
        project(&doc, &first);
        assert_eq!(doc.schema_version(), Some(6));
        assert!(doc.compatibility().writable());
    }
    // A Run created at 6 is not rewritten by a pinned save.
    let doc = bot6(false);
    doc.register_native_result(&pinned(0x58)).unwrap();
    assert_eq!(doc.schema_version(), Some(6));
}

// ---- newer and older builds -------------------------------------------------

#[test]
fn m6_run6_format_a_7_document_is_read_only() {
    for header in [bots::bot_header(), child_header()] {
        let doc = RunDocument::create(&header).unwrap();
        let waiting = (header.status == RunStatus::running()).then(|| child_waiting(&doc));
        doc.doc().get_map("run").insert("schema", 7i64).unwrap();
        doc.doc().commit();
        let before = doc.version();
        assert_eq!(
            doc.compatibility(),
            Compatibility::TooNew {
                found: 7,
                understood: 6
            }
        );
        assert_eq!(doc.header().unwrap(), header);
        let refused: Result<(), RunDocumentError> = Err(RunDocumentError::ReadOnly {
            found: 7,
            understood: 6,
        });
        assert_eq!(doc.append_step(&step(StepKind::Completed, 5)), refused);
        assert_eq!(doc.set_status(&RunStatus::Completed), refused);
        assert_eq!(doc.register_native_result(&pinned(0x58)), refused);
        if let Some(waiting) = waiting {
            assert_eq!(
                RunProjectionStaging::default()
                    .prepare(&doc, &waiting)
                    .err(),
                Some(RunDocumentError::ReadOnly {
                    found: 7,
                    understood: 6
                })
            );
        }
        assert_eq!(doc.version(), before);
    }
}

/// The format 5 vocabulary, frozen as the build before M6 had it
/// (`koru-delegate-core/src/bots.rs` and `status.rs` at `8fb7390a`). These are
/// deliberately not this build's types: the test shows what that build does.
mod format5 {
    use serde::Deserialize;
    use uuid::Uuid;

    #[derive(Deserialize, Debug)]
    #[serde(deny_unknown_fields)]
    #[allow(dead_code)]
    pub struct Owner {
        user_scope: Uuid,
        device: Uuid,
    }
    #[derive(Deserialize, Debug)]
    #[serde(deny_unknown_fields)]
    #[allow(dead_code)]
    pub struct Provenance {
        owner: Owner,
        bot: Uuid,
        submission: Uuid,
        chat_id: Uuid,
        ai_attempt_id: Option<Uuid>,
        assistant_message_id: Option<Uuid>,
        config_revision: u64,
        policy_revision: u64,
        continued_from: Option<Uuid>,
        #[serde(default)]
        routine: Option<serde_json::Value>,
    }
    #[derive(Deserialize, Debug)]
    #[serde(deny_unknown_fields)]
    #[allow(dead_code)]
    pub struct NativeResult {
        operation_id: Uuid,
        workspace: Uuid,
        document: Uuid,
        version: Vec<u8>,
        title: String,
        confirmed_at_ms: i64,
        kind: String,
    }
    #[derive(Deserialize, Debug, PartialEq, Eq, PartialOrd, Ord)]
    #[serde(rename_all = "snake_case")]
    pub enum Marker {
        ApprovalRequired,
        WaitingForInput,
        ChildSessionDetected,
        FinishedNeedingAttention,
    }
    #[derive(Deserialize, Debug)]
    #[serde(tag = "state", rename_all = "snake_case")]
    #[allow(dead_code)]
    pub enum Status {
        Running {
            #[serde(default)]
            attention: std::collections::BTreeSet<Marker>,
            #[serde(default)]
            control: serde_json::Value,
        },
        Created,
        Starting,
        Completed,
        Cancelled,
    }
    /// The reader maximum of that build.
    pub const UNDERSTOOD: i64 = 5;
}

#[test]
fn m6_run6_format_the_format_5_types_refuse_a_parent_a_pinned_result_and_the_marker() {
    // A child Run: `bot.parent` is an unknown field to the format 5 type.
    let child = RunDocument::create(&child_header()).unwrap();
    assert!(serde_json::from_value::<format5::Provenance>(stored(&child, "bot")).is_err());
    // A pinned result: `version_id` is an unknown field.
    let saved = bot6(false);
    saved.register_native_result(&pinned(0x58)).unwrap();
    let results = stored(&saved, "native_results");
    let entries = results.as_array().unwrap();
    assert!(serde_json::from_value::<format5::NativeResult>(entries[0].clone()).is_ok());
    assert!(serde_json::from_value::<format5::NativeResult>(entries[1].clone()).is_err());
    // A waiting parent: `waiting_for_bot` is not in the closed marker set.
    let waiting = bot6(false);
    project(&waiting, &child_waiting(&waiting));
    assert!(serde_json::from_value::<format5::Status>(stored(&waiting, "status")).is_err());
    // Each of the three is stamped above what that build writes, so it gates
    // every one of them read-only before it meets the value it cannot decode,
    // and it has no path that reads one as an ordinary Run.
    for doc in [&child, &saved, &waiting] {
        let found = doc.schema_version().unwrap();
        assert_eq!(found, 6);
        assert_eq!(
            Compatibility::against(Some(found), format5::UNDERSTOOD),
            Compatibility::TooNew {
                found: 6,
                understood: 5
            }
        );
        assert!(stored(doc, "bot").is_object());
    }
    // A format 6 Run that uses none of the three still decodes with the
    // format 5 types: it is only read-only there.
    let plain = bot6(false);
    assert!(serde_json::from_value::<format5::Provenance>(stored(&plain, "bot")).is_ok());
    assert!(serde_json::from_value::<format5::Status>(stored(&plain, "status")).is_ok());
    for entry in stored(&plain, "native_results").as_array().unwrap() {
        assert!(serde_json::from_value::<format5::NativeResult>(entry.clone()).is_ok());
    }
    // The new step is one entry that build reads past, not a refusal: an
    // unknown step kind is "a step this build does not understand".
    let step: serde_json::Value = literal(WAITING_STEP);
    assert_eq!(step["kind"]["step"], "waiting_for_bot");
    assert_eq!(
        crate::TimelineItem::decode(loro::LoroValue::from(serde_json::json!({
            "entry": "step",
            "id": "00000000-0000-4000-8000-00000000005b",
            "kind": {"step": "a_step_of_a_later_build"},
            "refs": [],
            "at_ms": 8
        }))),
        crate::TimelineItem::Unrecognized
    );
}
