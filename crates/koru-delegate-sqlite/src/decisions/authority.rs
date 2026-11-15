// SPDX-License-Identifier: MPL-2.0

use super::*;
use crate::{BotAdmissionRecord, attempts, bots::routines};

pub(super) fn owner(conn: &Connection, owner: BotOwner, run: RunId) -> Result<(), BotLedgerError> {
    let row=conn.query_row("SELECT CASE WHEN typeof(user_scope)='text' AND length(CAST(user_scope AS BLOB))=36 THEN user_scope END, CASE WHEN typeof(device_id)='text' AND length(CAST(device_id AS BLOB))=36 THEN device_id END FROM bot_admissions WHERE run_id=?1",[run.to_storage()],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?))).optional().map_err(sql)?;
    if row != Some((owner.user_scope.to_storage(), owner.device.to_storage())) {
        return Err(BotLedgerError::Fenced);
    }
    Ok(())
}

pub(super) fn retained(
    conn: &Connection,
    binding: &DecisionBindingV1,
) -> Result<BotAdmissionRecord, BotLedgerError> {
    let f = binding.origin();
    owner(conn, f.owner, f.run)?;
    routines::bounded_admission(conn, f.run)?;
    let admitted = routines::native_admission(conn, f.run)?;
    let (submission, policy, workspace, human, routine) = match binding {
        DecisionBindingV1::KoruEdit(v) => (
            v.submission,
            &v.captured_policy,
            v.workspace,
            v.human_profile,
            &v.routine,
        ),
        DecisionBindingV1::NativeApproval(v) => (
            v.submission,
            &v.captured_policy,
            v.workspace,
            v.human_profile,
            &v.routine,
        ),
    };
    let s = &admitted.submission;
    let (intent, request) = crate::intents::retained_bounded(conn, f.run)?;
    require(
        intent.state == crate::IntentState::Satisfied
            && intent.run_document.is_some()
            && intent.workspace == s.workspace
            && request == s.request,
    )?;
    require(
        s.provenance.owner == f.owner
            && s.provenance.bot == f.bot
            && s.provenance.submission == submission
            && s.request.run == f.run
            && s.policy == *policy
            && s.workspace == workspace
            && s.request.requested_by == ActorRef::Person { profile: human },
    )?;
    let (global, routine_policy) = match binding {
        DecisionBindingV1::KoruEdit(v) => (&v.global_policy, &v.routine_policy),
        DecisionBindingV1::NativeApproval(v) => (&v.global_policy, &v.routine_policy),
    };
    check_policy_bounds(conn, f.owner, f.bot, Some(global.key.request))?;
    let retained = crate::native_results::retained_policy_on(conn, &global.key)?
        .ok_or_else(|| corrupt("bot_decisions", "historical_policy"))?;
    require(
        global.config_revision.get() >= s.provenance.config_revision
            && global.policy_revision.get() >= s.provenance.policy_revision
            && global.config_revision.get() == retained.config_revision
            && global.policy_revision.get() == retained.policy_revision
            && global.digest.as_bytes() == &retained.payload_digest
            && retained.state == BotPolicyBarrierState::Effective
            && retained.effects_allowed,
    )?;
    if let Some(r) = routine_policy {
        let retained = routines::read_policy(conn, f.owner, f.bot, r.schedule, Some(r.request))?
            .ok_or_else(|| corrupt("bot_decisions", "historical_routine_policy"))?;
        require(
            r.grant_revision.get() == retained.grant_revision
                && r.digest == retained.payload_digest
                && retained.state == BotPolicyBarrierState::Effective
                && retained.effects_allowed,
        )?;
    }
    match (admitted.submission_version, &s.provenance.routine, routine) {
        (1, None, None) => routines::deny_run_metadata(conn, f.run)?,
        (2, Some(p), Some(r)) => {
            let claim = routines::validate_admission(conn, &admitted)?;
            require(
                r.provenance == *p
                    && r.claim == claim.claim
                    && routines::read(conn, p.schedule)? == Some(r.route.clone()),
            )?;
        }
        _ => return Err(BotLedgerError::Fenced),
    }
    let origin = attempt(conn, &f)?;
    require(origin.provider_session.and_then(|p| p.session) == Some(f.session))?;
    // Original first input is immutable. C2 adds the separately acknowledged
    // decision continuation owner; a newer attempt/turn alone is never enough.
    require(admitted.initial_send.as_ref().is_some_and(|s| {
        s.attempt == f.attempt && s.generation == f.generation && s.turn == Some(f.turn)
    }))?;
    Ok(admitted)
}
pub(super) fn attempt(
    conn: &Connection,
    f: &DecisionFence,
) -> Result<crate::AttemptRow, BotLedgerError> {
    let a = attempts::retained_bounded(conn, f.run, f.generation)?.ok_or(BotLedgerError::Fenced)?;
    require(a.attempt == f.attempt)?;
    Ok(a)
}

/// Durable local proof only. Current Store rights/Profile and original monotonic
/// deadline are checked by the future off-writer caller, never by a bool here.
pub(super) fn fresh(
    conn: &Connection,
    binding: &DecisionBindingV1,
    proof: &FreshBotPolicyProof,
    live: bool,
) -> Result<BotAdmissionRecord, BotLedgerError> {
    let admitted = retained(conn, binding)?;
    // The reused policy owner predates bounded body projections. Bound the
    // one exact current row before its existing decoder allocates strings/blob.
    let f = binding.origin();
    check_policy_bounds(conn, f.owner, f.bot, None)?;
    let (global, routine) = crate::native_results::current_composite(conn, &admitted, proof)?;
    let (g, r) = match binding {
        DecisionBindingV1::KoruEdit(v) => (&v.global_policy, &v.routine_policy),
        DecisionBindingV1::NativeApproval(v) => (&v.global_policy, &v.routine_policy),
    };
    require(
        g.key == global.key
            && g.config_revision.get() == global.config_revision
            && g.policy_revision.get() == global.policy_revision
            && g.digest.as_bytes() == &global.payload_digest,
    )?;
    match (r, routine) {
        (None, None) => {}
        (Some(r), Some(stored)) => require(
            r.owner == stored.owner
                && r.bot == stored.bot
                && r.schedule == stored.schedule
                && r.request == stored.request
                && r.grant_revision.get() == stored.grant_revision
                && r.digest == stored.payload_digest,
        )?,
        _ => return Err(BotLedgerError::Fenced),
    }
    if let Some(r) = &admitted.submission.provenance.routine {
        let claim = routines::validate_admission(conn, &admitted)?;
        require(
            matches!(claim.link_state, RoutineLinkState::Linked {})
                && claim.claim.slot.schedule == r.schedule,
        )?;
    }
    require(admitted.state == BotAdmissionState::Executing)?;
    let original = attempt(conn, &f)?;
    let current_generation: u32 = conn
        .query_row(
            "SELECT max(generation) FROM run_attempts WHERE run_id=?1",
            [f.run.to_storage()],
            |r| r.get(0),
        )
        .map_err(sql)?;
    require(original.generation.get() == current_generation)?;
    if live {
        require(!original.retired && original.phase == crate::AttemptPhase::Running)?;
    }
    Ok(admitted)
}

fn check_policy_bounds(
    conn: &Connection,
    owner: BotOwner,
    bot: BotId,
    request: Option<NativePolicyRequestId>,
) -> Result<(), BotLedgerError> {
    let fields = &[
        ("user_scope", "text", 36, false),
        ("device_id", "text", 36, false),
        ("bot_id", "text", 36, false),
        ("request_id", "text", 36, false),
        ("config_revision", "integer", 0, false),
        ("policy_revision", "integer", 0, false),
        ("payload_digest", "blob", 32, false),
        ("effects_allowed", "integer", 0, false),
        ("state", "text", 32, false),
    ];
    conn.query_row(&rows::query("bot_policy_barriers",fields,"user_scope=?1 AND device_id=?2 AND bot_id=?3 AND (?4 IS NULL OR request_id=?4) ORDER BY config_revision DESC LIMIT 1"),
        params![owner.user_scope.to_storage(),owner.device.to_storage(),bot.to_storage(),request.map(|r|r.to_storage())],|row|Ok(rows::Cells{row,table:"bot_policy_barriers"}.check()))
        .optional().map_err(sql)?.ok_or(BotLedgerError::Fenced)??;
    Ok(())
}

pub(super) fn selected_denial(
    conn: &Connection,
    binding: &DecisionBindingV1,
    proof: &FreshBotPolicyProof,
    admitted: &BotAdmissionRecord,
) -> Result<(), BotLedgerError> {
    let DecisionBindingV1::KoruEdit(v) = binding else {
        return Ok(());
    };
    // v2's captured/current policy is the independent Routine setup grant.
    // Native leases do not use global selected-page access for that subject;
    // do not invent a denial or turn its grant refusal into a one-use bypass.
    if proof.routine.is_some() || v.routine.is_some() {
        return Err(BotLedgerError::Fenced);
    }
    let current = &proof.global.policy;
    require(
        v.workspace == current.workspace
            && v.workspace == current.destination
            && v.workspace == v.captured_policy.destination,
    )?;
    let permits = |p: &CapturedBotPolicy| {
        p.access == BotAccess::Workspace || p.selected_pages.contains(&v.target)
    };
    if (permits(&v.captured_policy) && permits(current))
        || crate::native_results::created_page_on(conn, admitted, v.workspace, v.target)?
    {
        return Err(BotLedgerError::Fenced);
    }
    Ok(())
}
