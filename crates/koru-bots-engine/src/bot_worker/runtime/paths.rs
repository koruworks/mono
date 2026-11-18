// SPDX-License-Identifier: MPL-2.0
//! Provider resolvers do no I/O. The session port installs paths only after the
//! gateway has checked bytes and the engine's exact delivery authorization.
use crate::{Error, messages::Delivery, workspace::input::Prepared};
use koru_session_protocol::{AttachmentId, ErrorCode, SessionId};
use std::{collections::BTreeMap, path::PathBuf, sync::Mutex};
#[derive(Default)]
pub(super) struct Paths(Mutex<BTreeMap<(SessionId, AttachmentId), PathBuf>>);
impl Paths {
    pub fn install(&self, delivery: &Delivery, prepared: &Prepared) -> Result<(), Error> {
        prepared.validate(delivery)?;
        let session: SessionId = delivery
            .turn
            .session
            .to_string()
            .parse()
            .map_err(|_| Error::InvalidRequest)?;
        let mut paths = self.0.lock().map_err(|_| Error::StorageUnavailable)?;
        paths.retain(|(owner, _), _| *owner != session);
        for file in &prepared.files {
            if let Some(image) = &file.image {
                let id = file
                    .id
                    .to_string()
                    .parse()
                    .map_err(|_| Error::InvalidRequest)?;
                paths.insert((session, id), PathBuf::from(&image.path));
            }
        }
        Ok(())
    }
    pub fn remove(&self, session: uuid::Uuid) {
        if let Ok(mut paths) = self.0.lock() {
            paths.retain(|(owner, _), _| owner.as_uuid() != session);
        }
    }
}
impl koru_session_runtime::AttachmentPathResolver for Paths {
    fn resolve(&self, session: SessionId, attachment: AttachmentId) -> Result<PathBuf, ErrorCode> {
        self.0
            .lock()
            .map_err(|_| ErrorCode::DeliveryFailed)?
            .get(&(session, attachment))
            .cloned()
            .ok_or(ErrorCode::DeliveryFailed)
    }
}

#[cfg(test)]
mod tests;
