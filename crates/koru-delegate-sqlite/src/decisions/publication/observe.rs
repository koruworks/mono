// SPDX-License-Identifier: MPL-2.0

//! The delegate-owned decision-wait observation behind IPC16
//! `ReadBotDecisionWait`. Historical evidence (frozen intent, origin stop,
//! consumer cleanup, disposition, publication) and fresh eligibility are kept
//! apart: a retained enrollment, release or cleanup never renews authority.
use super::*;

/// One decision body, then at most the bounded open-prefix probe and one
/// projection body. Nothing walks older waits or decision history.
fn observe(
    conn: &Connection,
    owner: BotOwner,
    bot: BotId,
    run: RunId,
    wait_id: BotWaitId,
    decision: DecisionId,
) -> Result<Option<BotDecisionWaitObservation>, BotLedgerError> {
    authority::owner(conn, owner, run)?;
    let Some(record) = super::super::rows::read(conn, owner, run, decision)? else {
        return Ok(None);
    };
    let wait = &record.wait;
    // An existing decision under another wait or Bot is a subject mismatch,
    // never an absent observation.
    require(
        wait.wait == wait_id
            && wait.origin.owner == owner
            && wait.origin.bot == bot
            && wait.origin.run == run,
    )?;
    // Only an internal Koru origin wait-stop freezes a cleanup intent. Without
    // one there is nothing this route can describe (native waits never enroll).
    let Some(request) = wait.enrollment_request.clone() else {
        return Ok(None);
    };
    let key = &request.key;
    require(
        record.receipt.state.kind == DecisionKind::KoruEdit
            && key.owner == owner
            && key.bot == bot
            && key.run == run
            && key.wait_id == wait_id
            && key.decision_id == decision
            && request.cleanup_intent.origin() == wait.origin,
    )?;
    let state = &record.receipt.state;
    let enrolled_revision = match &wait.enrollment {
        None => key.wait_revision,
        Some(reply) => {
            require(
                matches!(reply, BotWaitEnrollmentReply::Enrolled { .. })
                    && reply.matches_request(key, false),
            )?;
            next(key.wait_revision)?
        }
    };
    // Fresh eligibility is derived only from today's exact state: the frozen
    // decision revision, the held wait at the frozen (or enrollment-recorded)
    // revision, and no release, cleanup or successor.
    let fresh_stop_eligibility =
        if record.receipt.closure.is_some() || wait.phase == BotWaitPhase::Settled {
            FreshWaitStopEligibility::Terminal
        } else if state.authorization == DecisionAuthorization::Pending
            && state.phase == DecisionPhase::Holding
            && state.revision == key.decision_revision
            && wait.phase == BotWaitPhase::HoldRequested
            && wait.revision == enrolled_revision
            && wait.release.is_none()
            && wait.cleanup.is_none()
            && record.resume.is_none()
        {
            FreshWaitStopEligibility::EligibleOriginWaitStop
        } else {
            FreshWaitStopEligibility::Superseded
        };
    // The row decoder already correlated the release with the frozen intent,
    // the recorded enrollment and the retired original attempt. The drain list
    // is rechecked against the native effect owner, not taken on trust.
    let origin_stop = match &wait.release {
        Some(release) => {
            let OriginWaitStop::Confirmed { native_drain, .. } = release else {
                return Err(corrupt("bot_wait_runtime", "release_evidence"));
            };
            super::super::drain::correlate(conn, owner, bot, native_drain)?;
            release.clone()
        }
        None => OriginWaitStop::Pending {},
    };
    let consumer_cleanup = match &wait.cleanup {
        Some(cleanup) => {
            let ConsumerCleanup::Confirmed { native_drain, .. } = cleanup else {
                return Err(corrupt("bot_wait_runtime", "cleanup_evidence"));
            };
            super::super::drain::correlate(conn, owner, bot, native_drain)?;
            cleanup.clone()
        }
        None if wait.phase == BotWaitPhase::CleanupPending => ConsumerCleanup::Owed {
            cleanup_revision: wait.revision,
            // A retained resume is the cleanup subject; the retired origin is
            // never substituted for a live or unknown successor.
            target: match &record.resume {
                Some(resume) => DecisionCleanupTarget::from_resume(resume)
                    .map_err(|_| corrupt("bot_decision_resumes", "cleanup_target"))?,
                None => DecisionCleanupTarget::Origin {
                    fence: wait.origin,
                    creation_key: authority::attempt(conn, &wait.origin)?
                        .provider_session
                        .ok_or(BotLedgerError::Fenced)?
                        .idempotency,
                },
            },
        },
        None => ConsumerCleanup::NotRequested {},
    };
    // The latest projection must belong to this wait. Once a later wait owns
    // the Run's newest projection, this wait's final target is not reachable
    // through a bounded read; that is reported, never answered from guesswork.
    rows::open(conn, run)?;
    let latest =
        rows::latest(conn, run)?.ok_or_else(|| corrupt("bot_decision_publications", "latest"))?;
    let latest = read(conn, owner, run, latest)?
        .ok_or_else(|| corrupt("bot_decision_publications", "latest"))?;
    if latest.identity.wait != wait_id {
        return Err(BotLedgerError::Stale);
    }
    let publication = match &latest.state {
        DecisionProjectionState::Applied {
            observed_version, ..
        } => DecisionPublication::Applied {
            transition: latest.identity.transition,
            sequence: latest.identity.sequence,
            target_digest: latest.identity.target_digest,
            observed_version: observed_version.clone(),
        },
        DecisionProjectionState::Pending | DecisionProjectionState::Prepared { .. } => {
            DecisionPublication::Pending {
                latest_sequence: latest.identity.sequence,
                target_digest: latest.identity.target_digest,
            }
        }
        DecisionProjectionState::Superseded { .. } => {
            return Err(corrupt("bot_decision_publications", "latest_state"));
        }
    };
    let observation = BotDecisionWaitObservation {
        key: request.key,
        cleanup_intent: request.cleanup_intent,
        current_revision: wait.revision,
        fresh_stop_eligibility,
        origin_stop,
        consumer_cleanup,
        disposition: control::disposition(&record.receipt)?,
        publication,
    };
    observation
        .validate()
        .map_err(|_| corrupt("bot_wait_runtime", "observation"))?;
    Ok(Some(observation))
}

impl RunsDb {
    /// Exact retained read for the delegate's `ReadBotDecisionWait`, without
    /// fresh policy or clock qualification; authentication is the caller's.
    /// `None` means this owner's Run positively has no such decision, or the
    /// decision has frozen no internal cleanup intent. A mismatched subject,
    /// corrupt evidence or an unreachable historical publication is an error.
    pub fn read_bot_decision_wait(
        &self,
        owner: BotOwner,
        bot: BotId,
        run: RunId,
        wait: BotWaitId,
        decision: DecisionId,
    ) -> Result<Option<BotDecisionWaitObservation>, BotLedgerError> {
        observe(self.conn(), owner, bot, run, wait, decision)
    }
}
