// SPDX-License-Identifier: MPL-2.0
//! Handwritten retained evidence exercises defenses; no executable Routine is enabled.
use super::*;
use crate::bots::routines::tests::convert;
use rusqlite::params;

fn converted(h: &mut Harness, linked: bool) -> koru_delegate_core::RoutineRouteIdentity {
    let mut s = h.db.bot_admission(h.fence.run).unwrap().unwrap().submission;
    convert(&mut h.db, &mut s, linked)
}
fn trigger(result: rusqlite::Result<usize>, expected: &str) {
    assert!(
        result.unwrap_err().to_string().contains(expected),
        "expected {expected}"
    );
}
fn companion(h: &Harness, r: &koru_delegate_core::RoutineRouteIdentity) -> rusqlite::Result<usize> {
    h.db.conn().execute("INSERT INTO native_routine_leases(run_id,operation_id,user_scope,device_id,bot_id,schedule_id,barrier_request_id,grant_revision,payload_digest) VALUES(?1,?2,?3,?4,?5,?6,?7,1,?8)",params![h.fence.run.to_storage(),h.request.key.operation_id.to_string(),r.owner.user_scope.to_storage(),r.owner.device.to_storage(),r.bot.to_storage(),r.schedule.to_storage(),h.policy.key.request.to_storage(),[7u8;32].as_slice()])
}
fn policy(h: &Harness, r: &koru_delegate_core::RoutineRouteIdentity) {
    h.db.conn().execute("INSERT INTO bot_routine_policy_barriers(user_scope,device_id,bot_id,schedule_id,request_id,grant_revision,payload_digest,effects_allowed,state,created_at_ms,updated_at_ms) VALUES(?1,?2,?3,?4,?5,1,?6,1,'effective',1000,1000)",params![r.owner.user_scope.to_storage(),r.owner.device.to_storage(),r.bot.to_storage(),r.schedule.to_storage(),h.policy.key.request.to_storage(),[7u8;32].as_slice()]).unwrap();
}
fn raw_lease(h: &Harness) -> rusqlite::Result<usize> {
    h.db.conn().execute("UPDATE native_result_intents SET lease_state='held',lease_id=?1,lease_user_scope=?2,lease_device_id=?3,lease_bot_id=?4,lease_barrier_request_id=?5,lease_config_revision=1,lease_policy_revision=1,lease_payload_digest=(SELECT payload_digest FROM bot_policy_barriers LIMIT 1)",params![Uuid::new_v4().to_string(),h.fence.owner.user_scope.to_storage(),h.fence.owner.device.to_storage(),h.fence.bot.to_storage(),h.policy.key.request.to_storage()])
}

#[test]
fn routine_sql_dispatch_gates_require_link_and_disallow_initial_execution() {
    let mut h = Harness::new();
    h.prepare();
    converted(&mut h, false);
    trigger(
        h.db.conn()
            .execute("UPDATE bot_admissions SET state='executing'", []),
        "routine_link_required",
    );
    trigger(
        h.db.conn().execute(
            "INSERT INTO bot_admissions SELECT * FROM bot_admissions",
            [],
        ),
        "routine_must_be_claimed_queued",
    );
    for invalid in [
        "UPDATE bot_routine_links SET definition_revision=0",
        "UPDATE bot_routine_links SET definition_revision=4294967296",
        "UPDATE bot_routine_links SET definition_digest='00000000000000000000000000000000'",
        "UPDATE bot_routine_links SET preparation_hash=x'00'",
        "UPDATE bot_routine_links SET owner_handoff='bad'",
        "UPDATE bot_routine_links SET grant_revision=0",
        "UPDATE bot_routine_links SET state='terminal'",
        "UPDATE bot_routine_links SET terminal_reason='stopped'",
        "UPDATE bot_routine_links SET user_scope='foreign'",
        "UPDATE bot_admissions SET submission_version=3",
        "DELETE FROM bot_routine_routes",
        "DELETE FROM schedule_claims",
        "DELETE FROM bot_admissions",
    ] {
        assert!(h.db.conn().execute(invalid, []).is_err(), "{invalid}");
    }
    h.db.conn()
        .execute("UPDATE bot_routine_links SET state='linked'", [])
        .unwrap();
    h.db.conn()
        .execute("UPDATE bot_admissions SET state='executing'", [])
        .unwrap();
    assert_eq!(h.db.reserve_bot(h.fence.run), Err(BotLedgerError::Fenced));
    assert_eq!(
        h.db.activate_bot(h.fence.run, h.fence.attempt, h.fence.generation),
        Err(BotLedgerError::Fenced)
    );
    assert_eq!(
        h.db.set_bot_active(h.fence.run, h.fence.attempt, h.fence.generation, true),
        Err(BotLedgerError::Fenced)
    );
    h.db.set_bot_active(h.fence.run, h.fence.attempt, h.fence.generation, false)
        .unwrap();
    let mut send =
        h.db.bot_admission(h.fence.run)
            .unwrap()
            .unwrap()
            .initial_send
            .unwrap();
    send.turn = None;
    assert_eq!(
        h.db.prepare_bot_initial_send(h.fence.run, &send),
        Err(BotLedgerError::Fenced)
    );
}

#[test]
fn routine_sql_native_companion_gates_bind_identity_and_retain_held_evidence() {
    let mut h = Harness::new();
    h.prepare();
    let r = converted(&mut h, true);
    policy(&h, &r);
    trigger(raw_lease(&h), "routine_lease_required");
    for mutation in [
        "UPDATE bot_routine_links SET state='held'",
        "UPDATE bot_routine_policy_barriers SET state='pending'",
        "UPDATE bot_routine_policy_barriers SET effects_allowed=0",
        "UPDATE bot_routine_policy_barriers SET payload_digest=zeroblob(32)",
        "UPDATE bot_routine_policy_barriers SET grant_revision=2",
    ] {
        h.db.conn().execute_batch("SAVEPOINT negative").unwrap();
        h.db.conn().execute(mutation, []).unwrap();
        trigger(companion(&h, &r), "routine_lease_identity");
        h.db.conn()
            .execute_batch("ROLLBACK TO negative; RELEASE negative;")
            .unwrap();
    }
    companion(&h, &r).unwrap();
    trigger(
        h.db.conn().execute(
            "UPDATE native_routine_leases SET grant_revision=grant_revision",
            [],
        ),
        "routine_lease_immutable",
    );
    raw_lease(&h).unwrap();
    trigger(
        h.db.conn().execute("DELETE FROM native_routine_leases", []),
        "routine_lease_held",
    );
    trigger(
        h.db.conn().execute(
            "INSERT INTO native_result_intents SELECT * FROM native_result_intents",
            [],
        ),
        "routine_lease_requires_prepared_intent",
    );
    assert!(
        h.db.conn()
            .execute("DELETE FROM bot_routine_policy_barriers", [])
            .is_err()
    );
    assert!(
        h.db.conn()
            .execute("DELETE FROM bot_routine_links", [])
            .is_err()
    );
    let replay =
        h.db.lease_native_result(
            &h.request.key,
            h.request.request_fingerprint,
            &h.fence,
            &h.policy,
        )
        .unwrap();
    assert!(replay.execute.is_none());
    h.confirm(NativeResultObservation::NotApplied {
        code: NativeResultRefusal::AccessDenied,
    });
    assert_eq!(
        h.db.conn()
            .execute("DELETE FROM native_routine_leases", [])
            .unwrap(),
        1
    );
}

#[test]
fn routine_native_fresh_authority_is_fenced_while_reconciliation_is_readable() {
    let mut h = Harness::new();
    let old = h.prepare();
    let r = converted(&mut h, true);
    policy(&h, &r);
    companion(&h, &r).unwrap();
    assert_eq!(h.prepare(), old);
    assert_eq!(h.db.native_result(&h.request.key).unwrap(), Some(old));
    assert_eq!(
        h.db.lease_native_result(
            &h.request.key,
            h.request.request_fingerprint,
            &h.fence,
            &h.policy
        ),
        Err(BotLedgerError::Fenced)
    );
    h.request.key.operation_id = Uuid::new_v4();
    assert_eq!(
        h.db.prepare_native_result(&h.request),
        Err(BotLedgerError::Fenced)
    );
    let request = super::memory::request(&h, true);
    assert_eq!(
        h.db.allocate_memory_edit(&request, fixtures::document(0xd0), &h.fence, &h.policy),
        Err(BotLedgerError::Fenced)
    );
    assert_eq!(
        h.db.conn()
            .query_row("SELECT count(*) FROM native_result_intents", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        h.db.conn()
            .query_row(
                "SELECT count(*) FROM bot_memory_edit_allocations",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
}
