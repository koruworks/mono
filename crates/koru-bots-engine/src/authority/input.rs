// SPDX-License-Identifier: MPL-2.0
//! Private gateway resolution of a claimed input, never a provider-supplied
//! list of file ids or paths. Reads the exact retained delivery under its turn.
use super::*;
use crate::{
    attachments,
    messages::{Delivery, Turn},
};

#[derive(Clone, Serialize, Deserialize)]
pub struct Grant {
    pub resolution: Resolution,
    pub delivery: Delivery,
    pub files: Vec<attachments::Record>,
}
impl std::fmt::Debug for Grant {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("InputGrant(<redacted>)")
    }
}
impl Database {
    /// Called only by the authenticated gateway. A session bearer alone does
    /// not choose file ownership or authorize another delivery/turn.
    pub fn resolve_input(
        &self,
        secret: &Secret,
        turn: Turn,
        delivery: Uuid,
    ) -> Result<Grant, Error> {
        let identity = identity(&self.connection, secret)?;
        let resolution = calls::open(&self.connection, &identity, Some(turn.id))?;
        if resolution.binding.turn != turn {
            return Err(Error::StaleTurn);
        }
        let bytes: Option<Vec<u8>> = self.connection.query_row(
            "SELECT payload FROM deliveries WHERE id=?1 AND turn=?2 AND state IN ('offered','accepted','unknown')",
            params![delivery.to_string(), turn.id.to_string()], |r| r.get(0),
        ).optional()?;
        let delivery: Delivery = decode(&bytes.ok_or(Error::NotAllowed)?)?;
        if delivery.turn != turn {
            return Err(Error::StaleTurn);
        }
        let mut files = vec![];
        let mut seen = std::collections::HashSet::new();
        for message in delivery.context.iter().chain(&delivery.work) {
            for value in &message.content.attachments {
                let attachment: koru_ai_core::ChatAttachment =
                    serde_json::from_value(value.clone()).map_err(|_| Error::StorageUnavailable)?;
                if attachment.message_id.map(|m| m.as_uuid()) != Some(message.id)
                    || !seen.insert(attachment.id)
                {
                    return Err(Error::NotAllowed);
                }
                let record = attachments::load(
                    &self.connection,
                    resolution.thread,
                    attachment.id.as_uuid(),
                )?
                .ok_or(Error::NotAllowed)?;
                if record.attachment != attachment {
                    return Err(Error::NotAllowed);
                }
                files.push(record);
            }
        }
        if files.len() > koru_ai_core::attachments::MAX_ATTACHMENTS_PER_MESSAGE {
            return Err(Error::InputTooSmall);
        }
        Ok(Grant {
            resolution,
            delivery,
            files,
        })
    }
}
