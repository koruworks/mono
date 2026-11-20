// SPDX-License-Identifier: MPL-2.0

//! §17.2: what a usage number means, and how it is recorded.
//!
//! The accounting unit is a turn, keyed by `(RunAttemptId, TurnId)`. A
//! provider's report arrives already normalized by its adapter to a total
//! for that turn; here it becomes a [`UsageFact`] with a stable identity
//! and a monotonic revision:
//!
//! - the first report for a turn mints the identity at revision 0;
//! - a report equal to the last one is a **retry**: nothing is appended to
//!   the document, and the publication is re-sent under the id its first
//!   delivery carried, so the journal dedupes it;
//! - a report that differs is a **correction**: the same identity, the
//!   next revision, a new event id, and `replay` picks it whatever order
//!   it arrives in;
//! - a report arriving after the turn ended, after the run completed, or
//!   from an attempt this device is no longer driving is **applied**, not
//!   dropped: a completed run's accounting is amended, never reopened.
//!
//! Completeness is declared, never assumed: an adapter that did not say
//! how `cached_input` relates to `input` yields an incomplete fact, and a
//! report with no counts at all says so rather than reading as zero.

use koru_delegate_core::{
    AgentTurnId, AmendmentRevision, Completeness, EventId, IncompleteReason, ModelIdentity,
    ModelName, ModelRef, ModelReport, RunId, UsageFact, UsageFactId,
};
use koru_delegate_sqlite::Projection;
use koru_session_protocol::{AgentUsage, CachedInputRelationship};

use crate::{RunFact, UsageReport, orchestrator::state::UsageIdentity};

use super::{Orchestrator, RunError};

impl Orchestrator {
    pub(crate) fn record_usage(
        &mut self,
        run: RunId,
        turn: AgentTurnId,
        usage: AgentUsage,
    ) -> Result<(), RunError> {
        let Some((attempt, _)) = self.open(run)?.attempt_ids() else {
            // A report for a run this device never drove names nothing it
            // can account under; the observable metrics still stand.
            return Ok(());
        };
        let model = self.model_ref(run, usage.model.as_ref().map(|name| name.as_str()))?;
        let completeness = match usage.cached_input {
            CachedInputRelationship::Undeclared
                if usage.cached_input_tokens.is_some() || usage.input_tokens.is_some() =>
            {
                Completeness::Incomplete {
                    reason: IncompleteReason::CachedInputRelationshipUndeclared,
                }
            }
            _ if usage.input_tokens.is_none()
                && usage.output_tokens.is_none()
                && usage.cached_input_tokens.is_none() =>
            {
                Completeness::Incomplete {
                    reason: IncompleteReason::NoUsageReported,
                }
            }
            _ => Completeness::Complete,
        };
        let totals = (
            usage.input_tokens,
            usage.output_tokens,
            usage.cached_input_tokens,
            model,
        );
        let key = (attempt, turn);
        let (identity, retry) = {
            let state = self.open(run)?;
            match state.usage.get_mut(&key) {
                Some(known) if known.last == totals => (known.clone(), true),
                Some(known) => {
                    known.revision = known.revision.next();
                    known.event = EventId::mint();
                    known.last = totals;
                    (known.clone(), false)
                }
                None => {
                    let fresh = UsageIdentity {
                        id: UsageFactId::mint(),
                        revision: AmendmentRevision::FIRST,
                        event: EventId::mint(),
                        last: totals,
                    };
                    state.usage.insert(key, fresh.clone());
                    (fresh, false)
                }
            }
        };
        let fact = UsageFact {
            identity: identity.id,
            attempt,
            turn,
            revision: identity.revision,
            input: usage.input_tokens,
            output: usage.output_tokens,
            cached_input: usage.cached_input_tokens,
            model,
            completeness,
        };
        if !retry {
            // Commit first (§17.1): the fact in the run's own storage, and
            // the header's summary refreshed from the replayed facts.
            let now = self.ports.clock.now_ms();
            let state = self.open(run)?;
            state.doc.commit(&mut |document| {
                document.record_usage(&fact)?;
                document.refresh_economics(now).map(|_| ())
            })?;
            let index = state.doc.document().timeline_len() as u64;
            self.ports
                .store
                .advance_cursor(run, Projection::HeaderSummary, index)?;
        }
        self.trace(
            run,
            "usage",
            format_args!(
                "turn {turn:?} revision {} retry {retry}",
                identity.revision.0
            ),
        );
        self.publish_as(
            run,
            identity.event,
            &RunFact::Usage {
                run,
                attempt,
                turn,
                report: UsageReport {
                    usage: identity.id,
                    revision: identity.revision,
                    input: usage.input_tokens,
                    output: usage.output_tokens,
                    cached_input: usage.cached_input_tokens,
                    model,
                    completeness,
                    cached_relationship: usage.cached_input,
                },
            },
        )
    }

    /// The reference to retained model metadata for what the provider
    /// said (§17.3): the model requested and the model reported, kept
    /// apart, with an explicit unknown for a provider that said nothing.
    fn model_ref(&mut self, run: RunId, reported: Option<&str>) -> Result<ModelRef, RunError> {
        let state = self.open(run)?;
        let provider = state
            .provider
            .clone()
            .unwrap_or_else(|| "unknown".parse().expect("a valid provider token"));
        let requested = state.requested_model.clone();
        let key = (
            provider.as_str().to_owned(),
            requested.clone(),
            reported.map(str::to_owned),
        );
        if let Some(existing) = state.models.get(&key) {
            return Ok(*existing);
        }
        let id = ModelRef::mint();
        let report = |name: Option<&str>| match name.and_then(|name| ModelName::new(name).ok()) {
            Some(name) => ModelReport::Named { name },
            None => ModelReport::Unknown,
        };
        let identity = ModelIdentity {
            id,
            provider,
            requested: report(requested.as_deref()),
            reported: report(reported),
        };
        state
            .doc
            .commit(&mut |document| document.record_model(&identity))?;
        state.models.insert(key, id);
        Ok(id)
    }
}
