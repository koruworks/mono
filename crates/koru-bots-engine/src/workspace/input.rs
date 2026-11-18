// SPDX-License-Identifier: MPL-2.0
//! Input resources resolved by the gateway under the exact engine delivery.
use crate::{Error, messages::Delivery};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Ten bounded paths plus native-copy paths and JSON escaping fit this reserve.
/// It is removed before the engine sizes a delivery, never truncated afterwards.
pub const RESOURCE_BYTES: usize = 192 * 1024;

/// Reserve exact known reference paths and a conservative bound for copied
/// file paths whose canonical root is known only by the gateway. No files
/// means no reserve: ordinary text/context capacity is unchanged.
pub(crate) fn budget(delivery: &Delivery) -> Result<usize, Error> {
    let mut bytes = 0;
    for value in delivery
        .context
        .iter()
        .chain(&delivery.work)
        .flat_map(|m| &m.content.attachments)
    {
        let attachment: koru_ai_core::ChatAttachment =
            serde_json::from_value(value.clone()).map_err(|_| Error::InvalidRequest)?;
        if bytes == 0 {
            bytes = 64;
        }
        bytes += if attachment.source == koru_ai_core::AttachmentSource::Path {
            serde_json::to_vec(&File {
                id: attachment.id.as_uuid(),
                kind: attachment.kind,
                path: attachment.reference_path.ok_or(Error::InvalidRequest)?,
                image: None,
            })
            .map_err(|_| Error::InvalidRequest)?
            .len()
                + 1
        } else {
            RESOURCE_BYTES / 10
        };
    }
    Ok(bytes)
}
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct File {
    pub id: Uuid,
    pub kind: koru_ai_core::AttachmentKind,
    pub path: String,
    pub image: Option<Image>,
}
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Image {
    pub path: String,
    pub media_type: String,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Prepared {
    pub delivery_hash: [u8; 32],
    pub files: Vec<File>,
}
impl std::fmt::Debug for Prepared {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PreparedInput(<redacted>)")
    }
}
pub fn digest(delivery: &Delivery) -> Result<[u8; 32], Error> {
    Ok(*blake3::hash(&serde_json::to_vec(delivery).map_err(|_| Error::InvalidRequest)?).as_bytes())
}
impl Prepared {
    pub fn empty(delivery: &Delivery) -> Result<Self, Error> {
        if delivery
            .context
            .iter()
            .chain(&delivery.work)
            .any(|m| !m.content.attachments.is_empty())
        {
            return Err(Error::NotAllowed);
        }
        Ok(Self {
            delivery_hash: digest(delivery)?,
            files: vec![],
        })
    }
    pub fn validate(&self, delivery: &Delivery) -> Result<(), Error> {
        if self.delivery_hash != digest(delivery)?
            || (!self.files.is_empty()
                && serde_json::to_vec(&self.files)
                    .map_err(|_| Error::InvalidRequest)?
                    .len()
                    + 32
                    > budget(delivery)?)
        {
            return Err(Error::InvalidRequest);
        }
        let expected: Vec<koru_ai_core::ChatAttachment> = delivery
            .context
            .iter()
            .chain(&delivery.work)
            .flat_map(|m| &m.content.attachments)
            .map(|value| {
                serde_json::from_value::<koru_ai_core::ChatAttachment>(value.clone())
                    .map_err(|_| Error::InvalidRequest)
            })
            .collect::<Result<_, _>>()?;
        if self.files.len() > 10 || expected.len() != self.files.len() {
            return Err(Error::InvalidRequest);
        }
        for (file, attachment) in self.files.iter().zip(expected) {
            if file.id != attachment.id.as_uuid()
                || file.kind != attachment.kind
                || (attachment.source == koru_ai_core::AttachmentSource::Path
                    && attachment.reference_path.as_deref() != Some(file.path.as_str()))
                || !valid_path(&file.path)
                || file.image.as_ref().is_some_and(|image| {
                    !valid_path(&image.path)
                        || !image.media_type.starts_with("image/")
                        || file.kind != koru_ai_core::AttachmentKind::Image
                })
            {
                return Err(Error::InvalidRequest);
            }
        }
        Ok(())
    }
}
pub fn valid_path(path: &str) -> bool {
    path.len() <= 4096
        && !path.chars().any(char::is_control)
        && std::path::Path::new(path).is_absolute()
}
