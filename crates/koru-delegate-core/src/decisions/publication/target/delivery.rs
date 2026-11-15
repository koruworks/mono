// SPDX-License-Identifier: MPL-2.0

use super::*;
use crate::{BotDeliveryReceipt, RunFact, StepKind};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionDeliveryOutcome {
    Retained,
    Completed,
    InterruptedUnknown,
}

/// Existing delivery identity. Runtime still qualifies actual native completion.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionProjectionDelivery {
    pub receipt: BotDeliveryReceipt,
    pub publication: EventId,
}

pub(super) fn validate(target: &RunProjectionTargetV1) -> Result<(), DecisionError> {
    let RunProjectionTargetV1::Control {
        action, delivery, ..
    } = target
    else {
        return require(!matches!(
            target.effects().event,
            Some(RunEvent::BotResponseDelivered)
        ));
    };
    let DecisionControlAction::Delivery { outcome } = action else {
        return require(delivery.is_none());
    };
    let delivery = delivery.as_ref().ok_or(DecisionError::Invalid)?;
    let fence = target.fence();
    require(
        delivery.receipt.valid()
            && delivery.receipt.run == fence.run
            && delivery.receipt.attempt == fence.attempt
            && delivery.receipt.generation == fence.generation,
    )?;
    if target.predecessor().status.is_terminal() || *outcome == DecisionDeliveryOutcome::Retained {
        return require(target.publications().is_empty());
    }
    match outcome {
        DecisionDeliveryOutcome::Completed => {
            require(
                target.effects().steps.as_slice().len() == 1 && target.publications().len() == 1,
            )?;
            let step = &target.effects().steps.as_slice()[0];
            let publication = &target.publications()[0];
            require(
                step.kind == StepKind::Completed
                    && publication.step == Some(step.id)
                    && publication.event == delivery.publication
                    && matches!(publication.fact, RunFact::Completed { run, attempt, generation, .. }
                    if run == fence.run && attempt == fence.attempt && generation == fence.generation),
            )
        }
        // The existing unknown-work owner records receipt + interruption but
        // does not claim Completed publication under this ID.
        DecisionDeliveryOutcome::InterruptedUnknown => require(
            target.publications().is_empty() && target.effects().steps.as_slice().len() == 1,
        ),
        DecisionDeliveryOutcome::Retained => unreachable!(),
    }
}
