// SPDX-License-Identifier: MPL-2.0

//! §7.1: creation, in order.
//!
//! 1. The creation intent, with the whole payload, keyed by the `RunId`
//!    the asking client minted. Until step 4 it is a pending request.
//! 2. Capture the context (§7.2). A capture that fails abandons the
//!    request with the item named, and no Run document exists.
//! 3. Commit the Run document. This is the moment a run exists.
//! 4. Mark the intent satisfied.
//!
//! A retry of a satisfied intent answers with the run it already made;
//! a restart completes an unsatisfied one forward from the payload
//! (`restart.rs`), through this same function.

use koru_delegate_core::{
    DelegationRequest, RunHeader, RunOrigin, StepKind, TimelineRef, TimelineRefs, WorkspaceId,
};
use koru_delegate_sqlite::IntentState;

use crate::{RunFact, orchestrator::state::RunState};

use super::{CreateError, Created, Orchestrator};

/// What the durable intent says before scope preflight.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CreateIntentStatus {
    Absent,
    Pending,
    Satisfied(Created),
}

impl Orchestrator {
    /// Return the durable answer for an exact satisfied retry before a host
    /// performs scope preflight. A changed payload is refused by the ledger;
    /// a new and a still-pending request remain distinct so the host can
    /// complete the already-durable request before gating only its start.
    pub fn existing_create(
        &mut self,
        request: &DelegationRequest,
        workspace: WorkspaceId,
    ) -> Result<CreateIntentStatus, CreateError> {
        refuse_untrusted_bot_folder(request)?;
        let Some(record) = self.ports.store.validate_intent(request, workspace)? else {
            return Ok(CreateIntentStatus::Absent);
        };
        match record.state {
            IntentState::Satisfied => {
                let document = record.run_document.ok_or_else(|| {
                    crate::PortError::new(
                        crate::Port::Store,
                        "corrupt",
                        "a satisfied intent names no document",
                    )
                })?;
                Ok(CreateIntentStatus::Satisfied(Created {
                    run: request.run,
                    workspace: record.workspace,
                    document,
                }))
            }
            IntentState::Abandoned => Err(CreateError::Port(crate::PortError::new(
                crate::Port::Store,
                "intent_abandoned",
                "the request was abandoned",
            ))),
            IntentState::Pending => Ok(CreateIntentStatus::Pending),
        }
    }

    pub fn create(
        &mut self,
        request: DelegationRequest,
        workspace: WorkspaceId,
    ) -> Result<Created, CreateError> {
        refuse_untrusted_bot_folder(&request)?;
        let run = request.run;
        // Step 1. Durable before anything else, and idempotent by run id.
        self.ports.store.record_intent(&request, workspace)?;
        if let Some(record) = self.ports.store.intent(run)? {
            match record.state {
                IntentState::Satisfied => {
                    let document = record.run_document.ok_or_else(|| {
                        crate::PortError::new(
                            crate::Port::Store,
                            "corrupt",
                            "a satisfied intent names no document",
                        )
                    })?;
                    return Ok(Created {
                        run,
                        workspace: record.workspace,
                        document,
                    });
                }
                IntentState::Abandoned => {
                    return Err(CreateError::Port(crate::PortError::new(
                        crate::Port::Store,
                        "intent_abandoned",
                        "the request was abandoned",
                    )));
                }
                IntentState::Pending => {}
            }
        }
        self.complete_intent(request, workspace)
    }

    /// Complete one intent that a schedule claim already wrote atomically.
    /// The durable payload is the authority: callers name only its RunId and
    /// cannot substitute a newer template, model, scope, or context.
    pub fn complete_claimed_intent(
        &mut self,
        run: koru_delegate_core::RunId,
    ) -> Result<Created, CreateError> {
        if let Some(pending) = self
            .ports
            .store
            .pending_intents()?
            .into_iter()
            .find(|pending| pending.run == run)
        {
            return self.complete_intent(pending.payload, pending.workspace);
        }
        let record = self.ports.store.intent(run)?.ok_or_else(|| {
            crate::PortError::new(
                crate::Port::Store,
                "unknown_intent",
                "the claimed run has no intent",
            )
        })?;
        match (record.state, record.run_document) {
            (IntentState::Satisfied, Some(document)) => Ok(Created {
                run,
                workspace: record.workspace,
                document,
            }),
            (IntentState::Abandoned, _) => Err(CreateError::Port(crate::PortError::new(
                crate::Port::Store,
                "intent_abandoned",
                "the claimed request was abandoned",
            ))),
            _ => Err(CreateError::Port(crate::PortError::new(
                crate::Port::Store,
                "corrupt",
                "the claimed intent has no document",
            ))),
        }
    }

    /// Steps 2–4, for a fresh request and for an intent found pending on
    /// restart alike.
    pub(crate) fn complete_intent(
        &mut self,
        request: DelegationRequest,
        workspace: WorkspaceId,
    ) -> Result<Created, CreateError> {
        let run = request.run;
        let bot = if matches!(
            request.scope,
            koru_delegate_core::WorkspaceScope::BotFolder { .. }
        ) {
            let admission = self
                .ports
                .store
                .bots()
                .ok_or_else(|| {
                    crate::PortError::new(
                        crate::Port::Store,
                        "bot_execution_unavailable",
                        "Bot admission is unavailable.",
                    )
                })?
                .bot_admission(run)
                .map_err(|_| {
                    crate::PortError::new(
                        crate::Port::Store,
                        "bot_admission_unreadable",
                        "Bot admission could not be read.",
                    )
                })?
                .filter(|record| {
                    record.submission.valid()
                        && record.submission.request == request
                        && record.submission.workspace == workspace
                })
                .ok_or_else(|| {
                    crate::PortError::new(
                        crate::Port::Store,
                        "bot_admission_required",
                        "A matching durable Bot admission is required.",
                    )
                })?;
            Some(admission.submission.provenance)
        } else {
            None
        };
        // Step 2. The pins are the identity the run keeps forever (UC2).
        let captured = match self.ports.capture.capture(run, &request.context, workspace) {
            Ok(captured) => captured,
            Err(failure) => {
                if request.origin == RunOrigin::Manual {
                    self.ports.store.abandon_intent(run)?;
                    self.trace(
                        run,
                        "abandoned",
                        format_args!("context item {} at {:?}", failure.item, failure.stage),
                    );
                } else {
                    // A scheduled intent and its slot claim were committed in
                    // one transaction. A second Store read can fail after
                    // that durable boundary; keeping the intent pending lets
                    // reconciliation complete it forward without inventing a
                    // second run or abandoning the claimed occurrence.
                    self.trace(run, "capture_retry", format_args!("scheduled context"));
                }
                return Err(CreateError::Capture(failure));
            }
        };
        let context_items = u32::try_from(captured.pins.len()).unwrap_or(u32::MAX);
        let origin_ref = match request.origin {
            RunOrigin::Manual => TimelineRefs::default(),
            RunOrigin::Scheduled {
                schedule,
                occurrence_ms,
                revision,
            } => TimelineRefs::new([TimelineRef::Schedule {
                schedule,
                occurrence_ms,
                revision,
            }])
            .expect("one schedule ref"),
        };
        let now = self.ports.clock.now_ms();
        let mut header = RunHeader::from_request(request, now);
        header.bot = bot;
        header.context = captured.pins;
        // Step 3. Idempotent by run id in the port; a retry after a lost
        // answer finds the document it made.
        let document = self.ports.documents.create(workspace, &header)?;
        let doc = self.ports.documents.open(workspace, document)?;
        self.runs
            .insert(run, RunState::new(workspace, document, doc));
        self.step(
            run,
            StepKind::RunCreated,
            origin_ref,
            Some(RunFact::Created {
                run,
                workspace,
                document,
                context_items,
            }),
        )
        .map_err(|error| match error {
            super::RunError::Port(port) => CreateError::Port(port),
            other => CreateError::Port(crate::PortError::new(
                crate::Port::Documents,
                other.code(),
                other.to_string(),
            )),
        })?;
        // Step 4.
        self.ports.store.satisfy_intent(run, document)?;
        self.trace(
            run,
            "created",
            format_args!("{context_items} context items"),
        );
        Ok(Created {
            run,
            workspace,
            document,
        })
    }
}

fn refuse_untrusted_bot_folder(request: &DelegationRequest) -> Result<(), CreateError> {
    if matches!(
        request.scope,
        koru_delegate_core::WorkspaceScope::BotFolder { .. }
    ) {
        return Err(CreateError::Port(crate::PortError::new(
            crate::Port::Store,
            "bot_admission_required",
            "Bot work requires trusted admission",
        )));
    }
    Ok(())
}
