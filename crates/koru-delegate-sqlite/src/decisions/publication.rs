// SPDX-License-Identifier: MPL-2.0

//! Sole bounded projection row owner. Document imports and semantic causal
//! coverage stay in the document adapter; no raw delta is a SQL authority.

use super::*;
mod insert;
mod control;
mod initial;
mod lifecycle;
mod cleanup_owner;
mod rows;
mod write;

pub(super) fn read(
    conn: &Connection,
    owner: BotOwner,
    run: RunId,
    transition: DecisionTransitionId,
) -> Result<Option<DecisionProjectionRecord>, BotLedgerError> {
    authority::owner(conn, owner, run)?;
    let record = rows::read(conn, run, transition)?;
    if let Some(record) = &record {
        correlate(conn, owner, record)?;
    }
    Ok(record)
}

fn correlate(
    conn: &Connection,
    owner: BotOwner,
    record: &DecisionProjectionRecord,
) -> Result<(), BotLedgerError> {
    let identity = &record.identity;
    let fence = record.target.fence();
    require(fence.owner == owner && fence.run == identity.run)?;
    let (intent, request) = crate::intents::retained_bounded(conn, identity.run)?;
    require(
        intent.state == crate::IntentState::Satisfied
            && intent.workspace == identity.workspace
            && intent.run_document == Some(identity.document),
    )?;
    crate::bots::routines::bounded_admission(conn, identity.run)?;
    let admission = crate::bots::routines::native_admission(conn, identity.run)?;
    require(
        admission.submission.request == request
            && admission.submission.provenance.owner == owner
            && admission.submission.provenance.bot == fence.bot
            && admission.submission.workspace == identity.workspace,
    )?;
    let wait = super::rows::by_wait(conn, identity.run, identity.wait)?
        .ok_or_else(|| corrupt("bot_decision_publications", "wait_id"))?;
    require(wait.origin.owner == owner && wait.origin.bot == fence.bot)?;
    match (&identity.decision, &wait.reason) {
        (Some(revision), BotWaitReason::Decision { decision }) => {
            require(revision.decision == *decision)?;
            let decision = super::rows::read(conn, owner, identity.run, *decision)?
                .ok_or_else(|| corrupt("bot_decision_publications", "decision_id"))?;
            require(
                decision.wait.wait == identity.wait
                    && revision.revision <= decision.receipt.state.revision,
            )?;
            // A successor is not inferred from a larger generation. Its exact
            // acknowledged consumer fence is retained by the resume owner.
            if *fence != wait.origin {
                require(
                    decision
                        .resume
                        .as_ref()
                        .is_some_and(|resume| consumer_matches(resume, fence)),
                )?;
            }
        }
        (
            None,
            BotWaitReason::Capacity {}
            | BotWaitReason::NativeExpired {}
            | BotWaitReason::OverlappingPrompt {},
        ) => require(*fence == wait.origin)?,
        _ => return Err(corrupt("bot_decision_publications", "decision_id")),
    }
    if matches!(record.state, DecisionProjectionState::Applied { .. }) {
        for publication in record.target.publications() {
            let body = encoded(&publication.fact, DECISION_FRAME_BYTES)?;
            require(crate::publications::verify_exact_on(
                conn,
                publication.event,
                identity.run,
                publication.fact.publication_kind(),
                &body,
            )?)?;
        }
    }
    Ok(())
}

fn consumer_matches(resume: &DecisionResume, fence: &DecisionFence) -> bool {
    let (attempt, generation, session, _) = resume.consumer_identity();
    let turn = match &resume.state {
        DecisionResumeState::InputAcknowledged { acknowledgment }
        | DecisionResumeState::Closed {
            acknowledgment: Some(acknowledgment),
        } => Some(acknowledgment.turn),
        _ => None,
    };
    fence.owner == resume.origin.owner
        && fence.bot == resume.origin.bot
        && fence.run == resume.origin.run
        && fence.attempt == attempt
        && fence.generation == generation
        && Some(fence.session) == session
        && Some(fence.turn) == turn
}

pub(super) fn latest_applied_on(
    conn: &Connection,
    owner: BotOwner,
    run: RunId,
) -> Result<Option<DecisionProjectionRecord>, BotLedgerError> {
    authority::owner(conn, owner, run)?;
    rows::open(conn, run)?;
    let mut current = rows::latest(conn, run)?;
    for _ in 0..3 {
        let Some(id) = current else {
            return Ok(None);
        };
        let record = read(conn, owner, run, id)?
            .ok_or_else(|| corrupt("bot_decision_publications", "transition_id"))?;
        if matches!(record.state, DecisionProjectionState::Applied { .. }) {
            return Ok(Some(record));
        }
        if matches!(record.state, DecisionProjectionState::Superseded { .. }) {
            return Err(corrupt("bot_decision_publications", "latest_state"));
        }
        let Some(prior) = &record.target.predecessor().projection else {
            return Ok(None);
        };
        let previous = read(conn, owner, run, prior.transition)?
            .ok_or_else(|| corrupt("bot_decision_publications", "predecessor"))?;
        require(
            previous.identity.sequence == prior.sequence
                && previous.identity.target_digest == prior.target_digest,
        )?;
        current = Some(prior.transition);
    }
    Err(corrupt("bot_decision_publications", "predecessor_depth"))
}

pub(super) fn prefix(
    conn: &Connection,
    owner: BotOwner,
    run: RunId,
) -> Result<Vec<DecisionProjectionRecord>, BotLedgerError> {
    authority::owner(conn, owner, run)?;
    let keys = rows::open(conn, run)?;
    keys.into_iter()
        .map(|id| {
            read(conn, owner, run, id)?
                .ok_or_else(|| corrupt("bot_decision_publications", "transition_id"))
        })
        .collect()
}

impl RunsDb {
    pub fn decision_projection(
        &self,
        owner: BotOwner,
        run: RunId,
        transition: DecisionTransitionId,
    ) -> Result<Option<DecisionProjectionRecord>, BotLedgerError> {
        read(self.conn(), owner, run, transition)
    }

    /// At most two typed bodies, after the narrow three-row shape probe.
    pub fn decision_projection_prefix(
        &self,
        owner: BotOwner,
        run: RunId,
    ) -> Result<Vec<DecisionProjectionRecord>, BotLedgerError> {
        prefix(self.conn(), owner, run)
    }

    pub fn next_decision_projection(
        &self,
        owner: BotOwner,
        run: RunId,
    ) -> Result<Option<DecisionProjectionRecord>, BotLedgerError> {
        authority::owner(self.conn(), owner, run)?;
        rows::open(self.conn(), run)?
            .first()
            .copied()
            .map(|id| {
                read(self.conn(), owner, run, id)?
                    .ok_or_else(|| corrupt("bot_decision_publications", "transition_id"))
            })
            .transpose()
    }

    pub fn latest_applied_decision_projection(
        &self,
        owner: BotOwner,
        run: RunId,
    ) -> Result<Option<DecisionProjectionRecord>, BotLedgerError> {
        latest_applied_on(self.conn(), owner, run)
    }

    pub fn latest_decision_projection(
        &self,
        owner: BotOwner,
        run: RunId,
    ) -> Result<Option<DecisionProjectionRecord>, BotLedgerError> {
        authority::owner(self.conn(), owner, run)?;
        // A latest/applied base is never derived through an illegal open
        // prefix, even when a caller did not first request that prefix.
        rows::open(self.conn(), run)?;
        rows::latest(self.conn(), run)?
            .map(|id| {
                read(self.conn(), owner, run, id)?
                    .ok_or_else(|| corrupt("bot_decision_publications", "transition_id"))
            })
            .transpose()
    }
}
