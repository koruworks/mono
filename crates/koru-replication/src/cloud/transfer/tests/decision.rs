// SPDX-License-Identifier: MPL-2.0

//! The `decision_required` policy amendment, against the real codec and the
//! real stream state machine.
//!
//! `decision.json` holds the amendment's raw policy cases. [`Baseline`] is a
//! copy of the control shape exactly as it stood before the field existed, so
//! the rows that depend on an older peer are pinned against that decoder's
//! actual behavior and not assumed.

use super::*;

/// `Control` as it was before `decision_required`, with its strict field set.
#[derive(serde::Deserialize, serde::Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
#[allow(clippy::enum_variant_names, dead_code)]
enum Baseline {
    TransferPolicy {
        version: Version1,
        policy_generation: i64,
        automatic_limit_bytes: usize,
    },
    TransferStart(Start),
    TransferDecision {
        version: Version1,
        policy_generation: i64,
        unit: Unit,
        batch_id: [u8; 8],
        sha256: Bytes<32>,
        decision: Decision,
    },
}
/// The baseline `decode_control`: the same length gate, strict shape and
/// closed policy domain.
fn baseline_decode(bytes: &[u8]) -> Result<Baseline, Error> {
    if bytes.is_empty() || bytes.len() > MAX_CONTROL {
        return Err(Error::Invalid);
    }
    let value: Baseline = serde_json::from_slice(bytes).map_err(|_| Error::Invalid)?;
    match &value {
        Baseline::TransferPolicy {
            policy_generation,
            automatic_limit_bytes,
            ..
        } if *policy_generation <= 0
            || ![0, RESTRICTED_LIMIT, MAX_PAYLOAD].contains(automatic_limit_bytes) =>
        {
            Err(Error::Invalid)
        }
        _ => Ok(value),
    }
}

const LEGACY_MAX: &str = r#"{"kind":"transfer_policy","version":1,"policy_generation":1,"automatic_limit_bytes":16777216}"#;
const REQUIRED_MAX: &str = r#"{"kind":"transfer_policy","version":1,"policy_generation":1,"automatic_limit_bytes":16777216,"decision_required":true}"#;

fn encoded(control: Control) -> String {
    String::from_utf8(encode_record(&Record::Control(control)).unwrap()[5..].to_vec()).unwrap()
}

/// A client (`a`, advertising the legacy shape) and a worker (`b`) whose
/// policy requires its decision when `deciding`, with policies exchanged
/// through the real codec.
fn pair_deciding(limit: usize, deciding: bool) -> (Stream, Stream, MemoryBudget) {
    let (mut a, _) = setup(RESTRICTED_LIMIT);
    let (b, budget) = setup(limit);
    let mut b = b.decision_required(deciding).unwrap();
    let ap = encoded(a.initial_policy(0).unwrap());
    let bp = encoded(b.initial_policy(0).unwrap());
    a.receive_control(decode_control(bp.as_bytes()).unwrap(), 0, 0, |_| Ok(()))
        .unwrap();
    b.receive_control(decode_control(ap.as_bytes()).unwrap(), 0, 0, |_| Ok(()))
        .unwrap();
    (a, b, budget)
}

fn begin(sender: &mut Stream, batch: [u8; 8], bytes: Vec<u8>, now: u64) -> (Start, Admission) {
    let (control, admission) = sender
        .begin_outgoing(unit(), batch, bytes, now, now, |_| Ok(()))
        .unwrap();
    let Control::TransferStart(start) = control else {
        panic!()
    };
    (start, admission)
}

/// The body frames an automatic sender would release for the same transfer:
/// what a sender that did not wait for its decision puts on the wire.
fn eager_frames(batch: [u8; 8], bytes: Vec<u8>) -> Vec<PeerFrame> {
    let (mut sender, _, _) = pair_deciding(MAX_PAYLOAD, false);
    let (_, admission) = begin(&mut sender, batch, bytes, 1);
    assert_eq!(admission, Admission::Automatic);
    let mut frames = Vec::new();
    while let Some(record) = sender.next_outgoing(1, |_| Ok(())).unwrap() {
        let Record::Peer(frame) = record.record() else {
            panic!()
        };
        frames.push(frame.clone());
    }
    frames
}

fn decision(start: &Start, edit: impl FnOnce(&mut Start)) -> Control {
    let mut changed = start.clone();
    edit(&mut changed);
    Control::decision(&changed, Decision::Accept)
}

#[test]
fn amendment_policy_cases_decode_at_the_real_and_the_baseline_boundary() {
    let vectors: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../../../contracts/vectors/cloud-transfer-v1/decision.json"
    ))
    .unwrap();
    let cases = vectors["cases"].as_array().unwrap();
    assert_eq!(cases.len(), 14);
    for case in cases {
        let name = case["name"].as_str().unwrap();
        let json = case["json"].as_str().unwrap();
        let decoded = decode_control(json.as_bytes());
        assert_eq!(decoded.is_ok(), case["valid"].as_bool().unwrap(), "{name}");
        assert_eq!(
            baseline_decode(json.as_bytes()).is_ok(),
            case["baseline_valid"].as_bool().unwrap(),
            "{name}: baseline"
        );
        let Ok(control) = decoded else { continue };
        let Control::TransferPolicy {
            automatic_limit_bytes,
            decision_required,
            ..
        } = control
        else {
            panic!("{name}")
        };
        let mode = match (automatic_limit_bytes, decision_required) {
            (0, _) => "paused",
            (_, true) => "decision_required",
            (_, false) => "automatic",
        };
        assert_eq!(mode, case["mode"], "{name}");
        // The mode a sender actually acts on, not only the decoded field.
        let (mut sender, _) = setup(RESTRICTED_LIMIT);
        sender.initial_policy(0).unwrap();
        sender.receive_control(control, 0, 0, |_| Ok(())).unwrap();
        assert_eq!(
            sender.peer_decision_required(),
            mode == "decision_required",
            "{name}"
        );
        let begun = sender.begin_outgoing(unit(), [1; 8], vec![0], 1, 1, |_| Ok(()));
        match mode {
            "paused" => assert_eq!(code(begun), "sync_unavailable", "{name}"),
            "automatic" => assert_eq!(begun.unwrap().1, Admission::Automatic, "{name}"),
            _ => assert_eq!(begun.unwrap().1, Admission::AwaitDecision, "{name}"),
        }
    }
}

#[test]
fn false_is_omitted_byte_identically_and_true_is_appended_last() {
    // The exact bytes a build before the field wrote, for every legal ceiling.
    for limit in [0, RESTRICTED_LIMIT, MAX_PAYLOAD] {
        let legacy = format!(
            r#"{{"kind":"transfer_policy","version":1,"policy_generation":7,"automatic_limit_bytes":{limit}}}"#
        );
        assert_eq!(encoded(Control::policy(7, limit)), legacy);
        assert_eq!(encoded(Control::policy_deciding(7, limit, false)), legacy);
        // The baseline encoder wrote these bytes and its decoder reads ours.
        let baseline = Baseline::TransferPolicy {
            version: Version1,
            policy_generation: 7,
            automatic_limit_bytes: limit,
        };
        assert_eq!(serde_json::to_string(&baseline).unwrap(), legacy);
        assert!(baseline_decode(legacy.as_bytes()).is_ok());
        // Explicit false is accepted and normalizes to the same bytes.
        let explicit = legacy.replace('}', r#","decision_required":false}"#);
        let decoded = decode_control(explicit.as_bytes()).unwrap();
        assert!(decoded == Control::policy(7, limit));
        assert_eq!(encoded(decoded), legacy);
        assert_eq!(
            encoded(Control::policy_deciding(7, limit, true)),
            legacy.replace('}', r#","decision_required":true}"#)
        );
    }
    assert_eq!(encoded(Control::policy(1, MAX_PAYLOAD)), LEGACY_MAX);
    assert_eq!(
        encoded(Control::policy_deciding(1, MAX_PAYLOAD, true)),
        REQUIRED_MAX
    );
    // A stream that never asked for the mode writes the legacy bytes, first
    // policy and every later generation.
    let (mut stream, _) = setup(MAX_PAYLOAD);
    assert_eq!(encoded(stream.initial_policy(0).unwrap()), LEGACY_MAX);
    assert_eq!(
        encoded(stream.change_policy(MAX_PAYLOAD, 1).unwrap()),
        LEGACY_MAX.replace("\"policy_generation\":1", "\"policy_generation\":2")
    );
    let (stream, _) = setup(MAX_PAYLOAD);
    let mut stream = stream.decision_required(false).unwrap();
    assert_eq!(encoded(stream.initial_policy(0).unwrap()), LEGACY_MAX);
    let (stream, _) = setup(MAX_PAYLOAD);
    let mut stream = stream.decision_required(true).unwrap();
    assert_eq!(encoded(stream.initial_policy(0).unwrap()), REQUIRED_MAX);
    // The mode is fixed before the first policy; afterwards it only moves
    // with a generation.
    assert_eq!(
        code(stream.decision_required(false)),
        "sync_transfer_invalid"
    );
}

#[test]
fn an_older_strict_decoder_refuses_the_required_policy_before_any_content() {
    // Old client / new worker, field true: the policy itself is refused, so
    // the old stream never reaches a start, a body or an Ack.
    assert_eq!(
        code(baseline_decode(REQUIRED_MAX.as_bytes())),
        "sync_transfer_invalid"
    );
    assert!(decode_control(REQUIRED_MAX.as_bytes()).is_ok());
    // Old client / new worker, field omitted: the existing behavior.
    assert!(baseline_decode(LEGACY_MAX.as_bytes()).is_ok());
    // New client / old worker: the old policy bytes mean false, and the old
    // automatic and above-ceiling behavior is unchanged.
    for (limit, length, expected) in [
        (MAX_PAYLOAD, 1, Admission::Automatic),
        (RESTRICTED_LIMIT, RESTRICTED_LIMIT, Admission::Automatic),
        (
            RESTRICTED_LIMIT,
            RESTRICTED_LIMIT + 1,
            Admission::AwaitDecision,
        ),
    ] {
        let (mut client, _) = setup(RESTRICTED_LIMIT);
        client.initial_policy(0).unwrap();
        let old = encoded(Control::policy(1, limit));
        assert!(!old.contains("decision_required"));
        client
            .receive_control(decode_control(old.as_bytes()).unwrap(), 0, 0, |_| Ok(()))
            .unwrap();
        assert!(!client.peer_decision_required());
        assert_eq!(begin(&mut client, [1; 8], vec![0; length], 1).1, expected);
    }
}

#[test]
fn required_mode_releases_no_body_before_the_exact_accept_at_either_size_bound() {
    for length in [1, MAX_PAYLOAD] {
        let (mut client, mut worker, budget) = pair_deciding(MAX_PAYLOAD, true);
        assert!(client.peer_decision_required());
        let (start, admission) = begin(&mut client, [1; 8], vec![3; length], 1);
        assert_eq!(admission, Admission::AwaitDecision);
        assert_eq!(start.total_length, length);
        let before = budget.used();
        assert_eq!(
            worker
                .receive_control(Control::TransferStart(start.clone()), 1, 1, |_| Ok(()))
                .unwrap(),
            Admission::AwaitDecision
        );
        // Nothing of the body is reserved while the start waits.
        assert_eq!(budget.used(), before);
        assert_eq!(worker.status().incoming_bytes, 0);
        let accept = worker
            .decide(&start, Decision::Accept, 2, |_| Ok(()))
            .unwrap();
        assert_eq!(
            client.receive_control(accept, 2, 2, |_| Ok(())).unwrap(),
            Admission::Accepted
        );
        while let Some(record) = client.next_outgoing(3, |_| Ok(())).unwrap() {
            let Record::Peer(frame) = record.record() else {
                panic!()
            };
            worker.receive_peer(frame.clone(), 3, |_| Ok(())).unwrap();
        }
        client.outbound_written(3).unwrap();
        let done = worker.handoff(4, |_| Ok(()), |done| done).unwrap();
        assert_eq!(done.bytes.len(), length);
        assert_eq!(done.batch_id, [1; 8]);
    }
    // The sender's own gate: asking for a frame before the Accept is invalid
    // and ends the stream; it is never a way to release one.
    for length in [1, MAX_PAYLOAD] {
        let (mut client, _, _) = pair_deciding(MAX_PAYLOAD, true);
        begin(&mut client, [1; 8], vec![3; length], 1);
        assert_eq!(
            code(client.next_outgoing(1, |_| Ok(()))),
            "sync_transfer_invalid"
        );
    }
}

#[test]
fn a_refused_required_start_reserves_no_body_and_opens_no_discard_window() {
    for length in [1, MAX_PAYLOAD] {
        let (mut client, mut worker, budget) = pair_deciding(MAX_PAYLOAD, true);
        let (start, _) = begin(&mut client, [2; 8], vec![5; length], 1);
        let before = budget.used();
        worker
            .reject_start_admission(start, 1, 1, |_| Ok(()))
            .unwrap();
        assert_eq!(budget.used(), before, "no receiver buffer for a refusal");
        assert_eq!(worker.status().incoming_bytes, 0);
        assert!(!worker.status().incoming_complete);
        assert_eq!(worker.status().closed, None);
        // Nothing can be handed off, so nothing can be acknowledged.
        let (mut probe, mut refused, _) = pair_deciding(MAX_PAYLOAD, true);
        let (again, _) = begin(&mut probe, [2; 8], vec![5; 8], 1);
        refused
            .reject_start_admission(again, 1, 1, |_| Ok(()))
            .unwrap();
        assert_eq!(
            code(refused.handoff(2, |_| Ok(()), |_| ())),
            "sync_transfer_invalid"
        );
        // The sender releases the attempt and keeps the stream.
        client.reject_outgoing_admission(2).unwrap();
        assert_eq!(client.status().outgoing_bytes, 0);
        assert_eq!(client.status().closed, None);
    }
    // In false mode an eager body in flight is discarded; in required mode
    // the same frame after a refusal is invalid, not an eager exception.
    let frames = eager_frames([2; 8], vec![5; 64]);
    let (mut client, mut worker, _) = pair_deciding(MAX_PAYLOAD, true);
    let (start, _) = begin(&mut client, [2; 8], vec![5; 64], 1);
    worker
        .reject_start_admission(start, 1, 1, |_| Ok(()))
        .unwrap();
    assert_eq!(
        code(worker.receive_peer(frames[0].clone(), 2, |_| Ok(()))),
        "sync_transfer_invalid"
    );
}

#[test]
fn a_body_before_the_accept_is_invalid_in_required_mode() {
    let frames = eager_frames([4; 8], vec![6; 64]);
    let (mut client, mut worker, _) = pair_deciding(MAX_PAYLOAD, true);
    let (start, _) = begin(&mut client, [4; 8], vec![6; 64], 1);
    assert_eq!(
        worker
            .receive_control(Control::TransferStart(start), 1, 1, |_| Ok(()))
            .unwrap(),
        Admission::AwaitDecision
    );
    assert_eq!(
        code(worker.receive_peer(frames[0].clone(), 1, |_| Ok(()))),
        "sync_transfer_invalid"
    );
    assert!(worker.status().closed.is_some());
    assert_eq!(
        code(worker.handoff(2, |_| Ok(()), |_| ())),
        "sync_transfer_invalid"
    );
}

#[test]
fn only_the_exact_pending_start_can_be_accepted_once() {
    let other = Unit::Document {
        workspace_id: id(1),
        document_id: id(6),
    };
    let wrong: [(&str, Box<dyn Fn(&mut Start)>); 4] = [
        ("unit", Box::new(move |s| s.unit = other)),
        ("generation", Box::new(|s| s.policy_generation = 2)),
        ("batch", Box::new(|s| s.batch_id = [9; 8])),
        ("digest", Box::new(|s| s.sha256 = Bytes([7; 32]))),
    ];
    for (name, edit) in wrong {
        let (mut client, _, _) = pair_deciding(MAX_PAYLOAD, true);
        let (start, _) = begin(&mut client, [1; 8], vec![1; 32], 1);
        assert_eq!(
            code(client.receive_control(decision(&start, edit), 2, 2, |_| Ok(()))),
            "sync_transfer_invalid",
            "{name}"
        );
        assert!(client.status().closed.is_some(), "{name}");
        assert_eq!(client.status().outgoing_bytes, 0, "{name}");
    }
    // Duplicate Accept.
    let (mut client, _, _) = pair_deciding(MAX_PAYLOAD, true);
    let (start, _) = begin(&mut client, [1; 8], vec![1; 32], 1);
    let accept = Control::decision(&start, Decision::Accept);
    client
        .receive_control(accept.clone(), 2, 2, |_| Ok(()))
        .unwrap();
    assert_eq!(
        code(client.receive_control(accept, 2, 2, |_| Ok(()))),
        "sync_transfer_invalid"
    );
    // Unsolicited: no start is pending at all.
    let (mut client, _, _) = pair_deciding(MAX_PAYLOAD, true);
    assert_eq!(
        code(client.receive_control(
            Control::decision(&start, Decision::Accept),
            2,
            2,
            |_| Ok(())
        )),
        "sync_transfer_invalid"
    );
    // Accept after Defer: Defer released the attempt and is not a success.
    let (mut client, _, _) = pair_deciding(MAX_PAYLOAD, true);
    let (start, _) = begin(&mut client, [1; 8], vec![1; 32], 1);
    assert_eq!(
        client
            .receive_control(Control::decision(&start, Decision::Defer), 2, 2, |_| Ok(()))
            .unwrap(),
        Admission::Deferred
    );
    assert_eq!(client.status().outgoing_bytes, 0);
    assert_eq!(
        code(client.receive_control(
            Control::decision(&start, Decision::Accept),
            3,
            3,
            |_| Ok(())
        )),
        "sync_transfer_invalid"
    );
    // Accept after the sender canceled the attempt.
    let (mut client, _, _) = pair_deciding(MAX_PAYLOAD, true);
    let (start, _) = begin(&mut client, [1; 8], vec![1; 32], 1);
    client.reject_outgoing_admission(2).unwrap();
    assert_eq!(
        code(client.receive_control(
            Control::decision(&start, Decision::Accept),
            3,
            3,
            |_| Ok(())
        )),
        "sync_transfer_invalid"
    );
    // Accept after the decision wait ran out: the timeout stands.
    let (mut client, _, _) = pair_deciding(MAX_PAYLOAD, true);
    let (start, _) = begin(&mut client, [1; 8], vec![1; 32], 1);
    let late = 1 + CONTROL_WAIT_MS;
    assert_eq!(
        code(client.receive_control(
            Control::decision(&start, Decision::Accept),
            late,
            late,
            |_| Ok(())
        )),
        "sync_transfer_timeout"
    );
    assert_eq!(client.status().outgoing_bytes, 0);
    // Accept after the receiving policy was replaced: the replacement
    // canceled the pending start and a late Accept cannot revive it.
    let (mut client, _, _) = pair_deciding(MAX_PAYLOAD, true);
    let (start, _) = begin(&mut client, [1; 8], vec![1; 32], 1);
    assert_eq!(
        code(client.receive_control(
            Control::policy_deciding(2, MAX_PAYLOAD, true),
            2,
            2,
            |_| Ok(())
        )),
        "sync_policy_changed"
    );
    assert_eq!(
        code(client.receive_control(
            Control::decision(&start, Decision::Accept),
            3,
            3,
            |_| Ok(())
        )),
        "sync_policy_changed"
    );
    assert_eq!(client.status().outgoing_bytes, 0);
    // False mode is unchanged: an automatic transfer must not get an Accept.
    let (mut client, _, _) = pair_deciding(MAX_PAYLOAD, false);
    let (start, admission) = begin(&mut client, [1; 8], vec![1; 32], 1);
    assert_eq!(admission, Admission::Automatic);
    assert_eq!(
        code(client.receive_control(
            Control::decision(&start, Decision::Accept),
            2,
            2,
            |_| Ok(())
        )),
        "sync_transfer_invalid"
    );
}

#[test]
fn the_receiver_decides_once_and_a_delayed_preflight_keeps_the_original_deadlines() {
    // The start's first byte owns the transfer budget; an Accept nine seconds
    // later moves neither that deadline nor the session expiry.
    let (mut client, mut worker, _) = pair_deciding(MAX_PAYLOAD, true);
    let (start, _) = begin(&mut client, [1; 8], vec![1; 32], 1000);
    let sender_deadline = client.outgoing_deadline().unwrap();
    assert_eq!(sender_deadline, 1000 + budget_millis(32).unwrap());
    worker
        .receive_control(
            Control::TransferStart(start.clone()),
            1000,
            1000,
            |_| Ok(()),
        )
        .unwrap();
    let expires = worker.expires_at();
    let accept = worker
        .decide(&start, Decision::Accept, 9999, |_| Ok(()))
        .unwrap();
    assert_eq!(
        worker.incoming_deadline(),
        Some(1000 + budget_millis(32).unwrap())
    );
    assert_eq!(worker.expires_at(), expires);
    client
        .receive_control(accept, 9999, 9999, |_| Ok(()))
        .unwrap();
    assert_eq!(client.outgoing_deadline(), Some(sender_deadline));
    // A second decision for the same start has nothing pending.
    assert_eq!(
        code(worker.decide(&start, Decision::Accept, 9999, |_| Ok(()))),
        "sync_transfer_invalid"
    );

    // Preflight that outlasts the control wait before the start is admitted.
    let (mut client, mut worker, _) = pair_deciding(MAX_PAYLOAD, true);
    let (start, _) = begin(&mut client, [1; 8], vec![1; 32], 1000);
    assert_eq!(
        code(worker.receive_control(
            Control::TransferStart(start),
            1000,
            1000 + CONTROL_WAIT_MS,
            |_| Ok(())
        )),
        "sync_transfer_timeout"
    );
    // Preflight that outlasts the decision wait after the start is pending.
    let (mut client, mut worker, _) = pair_deciding(MAX_PAYLOAD, true);
    let (start, _) = begin(&mut client, [1; 8], vec![1; 32], 1000);
    worker
        .receive_control(
            Control::TransferStart(start.clone()),
            1000,
            1000,
            |_| Ok(()),
        )
        .unwrap();
    assert_eq!(worker.next_deadline(), 1000 + CONTROL_WAIT_MS);
    assert_eq!(
        code(worker.decide(&start, Decision::Accept, 1000 + CONTROL_WAIT_MS, |_| Ok(()))),
        "sync_transfer_timeout"
    );
    assert!(worker.status().closed.is_some());
    // Preflight crossing the point where the credential no longer covers the
    // transfer budget: Accept rechecks the remaining lifetime and refuses.
    let (mut client, mut worker, _) = pair_deciding(MAX_PAYLOAD, true);
    let budget = budget_millis(32).unwrap();
    let edge = 600_000 - budget - 15_000;
    let (start, _) = begin(&mut client, [1; 8], vec![1; 32], edge - 5);
    worker
        .receive_control(
            Control::TransferStart(start.clone()),
            edge - 5,
            edge - 5,
            |_| Ok(()),
        )
        .unwrap();
    assert_eq!(
        code(worker.decide(&start, Decision::Accept, edge + 1, |_| Ok(()))),
        "sync_unavailable"
    );
    assert_eq!(worker.status().incoming_bytes, 0);
    // Authority lost during preflight: the Accept is refused with it.
    let (mut client, mut worker, _) = pair_deciding(MAX_PAYLOAD, true);
    let (start, _) = begin(&mut client, [1; 8], vec![1; 32], 1);
    worker
        .receive_control(Control::TransferStart(start.clone()), 1, 1, |_| Ok(()))
        .unwrap();
    assert_eq!(
        code(worker.decide(&start, Decision::Accept, 2, |_| {
            Err(Refusal::WorkspaceNotAuthorized)
        })),
        Refusal::WorkspaceNotAuthorized.code()
    );
    // The receiving policy changed during preflight: the waiting start is
    // discarded and the stream closes with the policy change.
    let (mut client, mut worker, _) = pair_deciding(MAX_PAYLOAD, true);
    let (start, _) = begin(&mut client, [1; 8], vec![1; 32], 1);
    worker
        .receive_control(Control::TransferStart(start.clone()), 1, 1, |_| Ok(()))
        .unwrap();
    assert_eq!(
        code(worker.change_policy_deciding(MAX_PAYLOAD, false, 2)),
        "sync_policy_changed"
    );
    assert_eq!(
        code(worker.decide(&start, Decision::Accept, 3, |_| Ok(()))),
        "sync_policy_changed"
    );
    // A drain discards the waiting start; only accepted work may finish.
    let (mut client, mut worker, _) = pair_deciding(MAX_PAYLOAD, true);
    let (start, _) = begin(&mut client, [1; 8], vec![1; 32], 1);
    worker
        .receive_control(Control::TransferStart(start.clone()), 1, 1, |_| Ok(()))
        .unwrap();
    worker.drain();
    assert_eq!(
        code(worker.decide(&start, Decision::Accept, 2, |_| Ok(()))),
        "sync_transfer_invalid"
    );
    client.drain();
    assert_eq!(client.status().outgoing_bytes, 0);
}

#[test]
fn zero_stays_paused_with_the_mode_and_the_boolean_moves_only_with_a_generation() {
    // Zero with true: paused, no start, no manual bypass.
    let (mut client, mut worker, _) = pair_deciding(0, true);
    assert!(!client.peer_decision_required());
    assert_eq!(
        code(client.begin_outgoing(unit(), [0; 8], vec![0], 1, 1, |_| Ok(()))),
        "sync_unavailable"
    );
    assert_eq!(
        code(worker.receive_control(Control::TransferStart(start(1)), 1, 1, |_| Ok(()))),
        "sync_transfer_invalid"
    );

    // An idle direction turns the mode on with the next generation.
    let (mut client, mut worker, _) = pair_deciding(MAX_PAYLOAD, false);
    assert!(!client.peer_decision_required());
    let changed = worker.change_policy_deciding(MAX_PAYLOAD, true, 1).unwrap();
    assert_eq!(
        encoded(changed.clone()),
        REQUIRED_MAX.replace("\"policy_generation\":1", "\"policy_generation\":2")
    );
    assert_eq!(
        client
            .receive_control(changed.clone(), 1, 1, |_| Ok(()))
            .unwrap(),
        Admission::Policy
    );
    assert!(client.peer_decision_required());
    assert_eq!(client.status().peer_generation, Some(2));
    // A duplicate of the same normalized identity is a no-op.
    assert_eq!(
        client.receive_control(changed, 1, 1, |_| Ok(())).unwrap(),
        Admission::Policy
    );
    assert_eq!(client.status().peer_generation, Some(2));
    // An equal-ceiling `change_policy` keeps the mode it has.
    let kept = worker.change_policy(MAX_PAYLOAD, 2).unwrap();
    assert!(encoded(kept.clone()).ends_with(r#","decision_required":true}"#));
    client.receive_control(kept, 2, 2, |_| Ok(())).unwrap();
    assert!(client.peer_decision_required());
    // And off again, which a start then follows as automatic.
    let off = worker
        .change_policy_deciding(MAX_PAYLOAD, false, 3)
        .unwrap();
    client.receive_control(off, 3, 3, |_| Ok(())).unwrap();
    assert!(!client.peer_decision_required());
    assert_eq!(
        begin(&mut client, [1; 8], vec![0], 4).1,
        Admission::Automatic
    );

    // Same generation, changed boolean: invalid in both directions of change.
    for (first, second) in [(false, true), (true, false)] {
        let (mut client, _) = setup(RESTRICTED_LIMIT);
        client.initial_policy(0).unwrap();
        client
            .receive_control(
                Control::policy_deciding(1, MAX_PAYLOAD, first),
                0,
                0,
                |_| Ok(()),
            )
            .unwrap();
        assert_eq!(
            code(client.receive_control(
                Control::policy_deciding(1, MAX_PAYLOAD, second),
                1,
                1,
                |_| Ok(())
            )),
            "sync_transfer_invalid"
        );
        assert!(
            !client.peer_decision_required(),
            "a closed stream is not a mode"
        );
    }
    // Skipped and backtracked generations stay invalid with the field.
    for generation in [1, 4] {
        let (mut client, _) = setup(RESTRICTED_LIMIT);
        client.initial_policy(0).unwrap();
        client
            .receive_control(Control::policy(1, MAX_PAYLOAD), 0, 0, |_| Ok(()))
            .unwrap();
        client
            .receive_control(Control::policy_deciding(2, MAX_PAYLOAD, true), 0, 0, |_| {
                Ok(())
            })
            .unwrap();
        assert_eq!(
            code(client.receive_control(
                Control::policy_deciding(generation, MAX_PAYLOAD, false),
                1,
                1,
                |_| Ok(())
            )),
            "sync_transfer_invalid"
        );
    }
    // A first policy must be generation 1 whatever its mode.
    let (mut client, _) = setup(RESTRICTED_LIMIT);
    client.initial_policy(0).unwrap();
    assert_eq!(
        code(client.receive_control(
            Control::policy_deciding(2, MAX_PAYLOAD, true),
            0,
            0,
            |_| Ok(())
        )),
        "sync_transfer_invalid"
    );
}

#[test]
fn a_new_stream_learns_the_mode_anew_and_never_inherits_it() {
    let (earlier, _, _) = pair_deciding(MAX_PAYLOAD, true);
    assert!(earlier.peer_decision_required());
    // The replacement stream, before its peer has said anything.
    let (mut next, _) = setup(RESTRICTED_LIMIT);
    assert!(!next.peer_decision_required());
    next.initial_policy(0).unwrap();
    assert!(!next.peer_decision_required());
    // A replacement or mixed worker that omits the field.
    next.receive_control(decode_control(LEGACY_MAX.as_bytes()).unwrap(), 0, 0, |_| {
        Ok(())
    })
    .unwrap();
    assert!(!next.peer_decision_required());
    assert_eq!(next.status().peer_generation, Some(1));
    // The earlier stream's mode ends with it.
    let mut earlier = earlier;
    earlier.cancel(Error::Closed);
    assert!(!earlier.peer_decision_required());
}

#[test]
fn a_refused_required_upload_leaves_an_accepted_download_and_its_reservations_intact() {
    let (mut client, client_budget) = setup(RESTRICTED_LIMIT);
    let (worker, worker_budget) = setup(MAX_PAYLOAD);
    let mut worker = worker.decision_required(true).unwrap();
    let cp = client.initial_policy(0).unwrap();
    let wp = worker.initial_policy(0).unwrap();
    client.receive_control(wp, 0, 0, |_| Ok(())).unwrap();
    worker.receive_control(cp, 0, 0, |_| Ok(())).unwrap();
    let idle = (client_budget.used(), worker_budget.used());

    // An independently accepted download is under way.
    let (download, admission) = worker
        .begin_outgoing(
            unit(),
            [2; 8],
            vec![9; 3 * FRAGMENT_BYTES],
            1,
            1,
            |_| Ok(()),
        )
        .unwrap();
    assert_eq!(admission, Admission::AwaitDecision);
    let Control::TransferStart(download) = download else {
        panic!()
    };
    client
        .receive_control(Control::TransferStart(download.clone()), 1, 1, |_| Ok(()))
        .unwrap();
    let accept = client
        .decide(&download, Decision::Accept, 1, |_| Ok(()))
        .unwrap();
    worker.receive_control(accept, 1, 1, |_| Ok(())).unwrap();
    let first = worker.next_outgoing(2, |_| Ok(())).unwrap().unwrap();
    let Record::Peer(frame) = first.record() else {
        panic!()
    };
    client.receive_peer(frame.clone(), 2, |_| Ok(())).unwrap();
    drop(first);
    let download_deadline = client.incoming_deadline().unwrap();

    // The blocked retry crosses it and is refused on its metadata, twice.
    for batch in [[3; 8], [4; 8]] {
        let before = worker_budget.used();
        let (start, admission) = begin(&mut client, batch, vec![7; 2 * FRAGMENT_BYTES], 3);
        assert_eq!(admission, Admission::AwaitDecision);
        worker
            .reject_start_admission(start, 3, 3, |_| Ok(()))
            .unwrap();
        assert_eq!(worker_budget.used(), before);
        client.reject_outgoing_admission(3).unwrap();
        assert_eq!(client.status().outgoing_bytes, 0);
    }
    // The download kept its original deadline, finishes, and is handed off.
    assert_eq!(client.incoming_deadline(), Some(download_deadline));
    while let Some(record) = worker.next_outgoing(4, |_| Ok(())).unwrap() {
        let Record::Peer(frame) = record.record() else {
            panic!()
        };
        client.receive_peer(frame.clone(), 4, |_| Ok(())).unwrap();
    }
    worker.outbound_written(4).unwrap();
    let done = client.handoff(5, |_| Ok(()), |done| done).unwrap();
    assert_eq!(done.bytes, vec![9; 3 * FRAGMENT_BYTES]);
    assert_eq!(done.batch_id, [2; 8]);
    assert_eq!(
        (client.status().closed, worker.status().closed),
        (None, None)
    );
    // Every reservation, refused attempts included, was released once: the
    // budgets are back where they were, and nothing was released twice
    // (an underflow would not return to the idle value).
    drop(done);
    assert_eq!((client_budget.used(), worker_budget.used()), idle);
}

#[test]
fn an_unchanged_refusal_costs_one_start_and_no_body_each_cycle() {
    let (mut client, mut worker, budget) = pair_deciding(MAX_PAYLOAD, true);
    let idle = budget.used();
    let payload = vec![8; FRAGMENT_BYTES + 1];
    let mut starts = 0;
    for cycle in 0..4u64 {
        let now = 1 + cycle * 60_000;
        // The retry is offered only under the current required mode.
        assert!(client.peer_decision_required());
        let (start, admission) = begin(&mut client, [cycle as u8 + 1; 8], payload.clone(), now);
        assert_eq!(admission, Admission::AwaitDecision);
        starts += 1;
        worker
            .reject_start_admission(start, now, now, |_| Ok(()))
            .unwrap();
        assert_eq!(worker.status().incoming_bytes, 0);
        assert_eq!(budget.used(), idle, "cycle {cycle}");
        client.reject_outgoing_admission(now).unwrap();
        assert_eq!(client.status().outgoing_bytes, 0);
    }
    assert_eq!(starts, 4);
    // Resources recovered: the same intended start is admitted, its retained
    // body follows the exact Accept, and only then can it be handed off.
    let now = 1 + 4 * 60_000;
    let (start, _) = begin(&mut client, [9; 8], payload.clone(), now);
    worker
        .receive_control(Control::TransferStart(start.clone()), now, now, |_| Ok(()))
        .unwrap();
    let accept = worker
        .decide(&start, Decision::Accept, now, |_| Ok(()))
        .unwrap();
    client
        .receive_control(accept, now, now, |_| Ok(()))
        .unwrap();
    while let Some(record) = client.next_outgoing(now, |_| Ok(())).unwrap() {
        let Record::Peer(frame) = record.record() else {
            panic!()
        };
        worker.receive_peer(frame.clone(), now, |_| Ok(())).unwrap();
    }
    assert_eq!(
        worker.handoff(now, |_| Ok(()), |done| done.bytes).unwrap(),
        payload
    );
}
