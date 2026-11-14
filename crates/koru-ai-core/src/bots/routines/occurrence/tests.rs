// SPDX-License-Identifier: MPL-2.0
use super::*;
mod golden;

fn fixture() -> BotRoutinePreparation {
    let mut row = super::super::tests::preparation();
    row.capture.grant.digest = row.capture.grant.canonical_digest().unwrap();
    let p = &mut row.prepared;
    p.definition.digest = koru_delegate_schedule::routine_definition_digest(
        &row.capture.schedule_definition,
        row.capture.canonical.home,
        p.definition.owner_handoff,
    )
    .unwrap();
    p.submission.provenance.routine.as_mut().unwrap().definition = p.definition;
    p.semantic_hash = koru_delegate_schedule::routine_semantic_digest(
        &p.slot,
        &row.capture.schedule_definition,
        row.capture.canonical.home,
        p.definition.owner_handoff,
    )
    .unwrap();
    p.preparation_hash = bot_routine_preparation_digest(p, &row.capture).unwrap();
    row
}

#[test]
fn preparation_hash_excludes_only_its_own_digest_and_binds_private_capture() {
    let mut row = fixture();
    let expected = row.prepared.preparation_hash;
    row.prepared.preparation_hash = RoutineDigest32::from_bytes([99; 32]);
    assert_eq!(
        bot_routine_preparation_digest(&row.prepared, &row.capture).unwrap(),
        expected
    );
    row.capture.library.index_digest[0] ^= 1;
    assert_ne!(
        bot_routine_preparation_digest(&row.prepared, &row.capture).unwrap(),
        expected
    );
    row = fixture();
    row.prepared.semantic_hash = RoutineDigest32::from_bytes([88; 32]);
    assert!(bot_routine_preparation_digest(&row.prepared, &row.capture).is_err());
    row = fixture();
    row.prepared.definition.digest = RoutineDigest32::from_bytes([77; 32]);
    row.prepared
        .submission
        .provenance
        .routine
        .as_mut()
        .unwrap()
        .definition = row.prepared.definition;
    assert!(bot_routine_preparation_digest(&row.prepared, &row.capture).is_err());
}

#[test]
fn generic_terminal_is_never_a_public_stopped_preparation() {
    let generic = BotRoutineStoredClosure::ClaimedTerminal {
        reason: RoutineTerminalReason::AlreadyTerminal,
    };
    assert_eq!(generic.sql_reason(), BotRoutineCloseReason::Stopped);
    assert_eq!(generic.preparation_reason(), None);
    let stopped = BotRoutineStoredClosure::ClaimedTerminal {
        reason: RoutineTerminalReason::Stopped,
    };
    assert_eq!(
        stopped.preparation_reason(),
        Some(BotRoutineCloseReason::Stopped)
    );
    assert_ne!(
        serde_json::to_string(&generic).unwrap(),
        serde_json::to_string(&stopped).unwrap()
    );
    assert!(
        serde_json::from_str::<BotRoutineStoredClosure>(
            r#"{"kind":"claimed_terminal","reason":"already_terminal","claimed_at_ms":4}"#
        )
        .is_err()
    );
}
