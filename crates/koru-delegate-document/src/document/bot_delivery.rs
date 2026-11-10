// SPDX-License-Identifier: MPL-2.0

use super::*;
use koru_delegate_core::{BotDeliveryReceipt, RunEvent, StepKind, TimelineRef, transition};

impl RunDocument {
    fn validated_bot_delivery(&self, receipt: &BotDeliveryReceipt) -> Result<RunHeader> {
        self.writable()?;
        let mut header = self.header()?;
        if header
            .chat_delivery
            .as_ref()
            .is_some_and(|old| old != receipt)
        {
            return Err(RunDocumentError::Malformed { part: Part::Header });
        }
        header.chat_delivery = Some(receipt.clone());
        if !header.bot_fields_valid() || receipt.run != header.run {
            return Err(RunDocumentError::Malformed { part: Part::Header });
        }
        Ok(header)
    }

    /// Retain delivery history without changing success or any native result.
    /// Authorization and current execution proof belong to the delegate owner.
    pub fn record_bot_chat_delivery(&self, receipt: &BotDeliveryReceipt) -> Result<()> {
        let header = self.validated_bot_delivery(receipt)?;
        if self.header()?.chat_delivery == header.chat_delivery {
            return Ok(());
        }
        self.commit_value("chat_delivery", to_stored(receipt, Part::Header)?)
    }

    /// One owner update: exact receipt, explicit completion, and its lean step.
    /// An identical completed replay never appends another step.
    pub fn complete_bot_chat_delivery(
        &self,
        receipt: &BotDeliveryReceipt,
        entry: &TimelineEntry,
    ) -> Result<()> {
        let header = self.validated_bot_delivery(receipt)?;
        if header.status == RunStatus::Completed {
            if self.header()?.chat_delivery.as_ref() == Some(receipt) {
                return Ok(());
            }
            return Err(RunDocumentError::Malformed { part: Part::Header });
        }
        if entry.kind != StepKind::Completed
            || !entry.refs.iter().any(|reference| matches!(reference, TimelineRef::Attempt { attempt } if *attempt == receipt.attempt))
        {
            return Err(RunDocumentError::Malformed { part: Part::Timeline });
        }
        let next = transition(&header.status, &RunEvent::BotResponseDelivered)
            .map_err(|_| RunDocumentError::Malformed { part: Part::Header })?;
        let receipt = to_stored(receipt, Part::Header)?;
        let status = to_stored(&next, Part::Header)?;
        let entry = to_stored(&StoredItem::Step(entry.clone()), Part::Timeline)?;
        let run = self.run();
        run.insert("chat_delivery", receipt)?;
        run.insert(K_STATUS, status)?;
        run.delete("failure_reason")?;
        self.timeline().push(entry)?;
        self.doc.commit();
        Ok(())
    }
}
