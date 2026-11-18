// SPDX-License-Identifier: MPL-2.0
use super::*;
impl Gateway {
    pub(super) fn discard_attachment_files(&self, thread: Uuid, id: Uuid) {
        if files::discard(self.files.as_ref(), &self.root, thread, id).is_err() {
            tracing::warn!(target: "koru_bots::attachments", "attachment cleanup deferred to the next start");
        }
    }
    pub(in crate::bots_engine) fn recover_attachment_files(
        &self,
        started_ms: i64,
        stopping: impl Fn() -> bool,
    ) -> Result<(), Error> {
        let owner = self.authority.owner().map_err(authority_error)?;
        let scope = owner.user_scope.to_storage();
        let call = |command| {
            if self.authority.owner().map_err(authority_error)? != owner {
                return Err(Error::NotAllowed);
            }
            self.attachment_call(&scope, command)
        };
        let cutoff_ms =
            started_ms.saturating_sub(koru_ai_core::attachments::STAGED_ATTACHMENT_TTL_MS);
        let mut after = None;
        loop {
            if stopping() {
                return Ok(());
            }
            let Reply::Sweep(page) = call(Control::Expire { after, cutoff_ms })? else {
                return Err(Error::EngineUnavailable);
            };
            for (thread, id) in page.garbage {
                if stopping() {
                    return Ok(());
                }
                self.discard_attachment_files(thread, id);
            }
            let Some(next) = page.next else {
                break;
            };
            if after.is_some_and(|prior| next <= prior) {
                return Err(Error::EngineUnavailable);
            }
            after = Some(next);
        }
        files::sweep(
            self.files.as_ref(),
            &self.root,
            started_ms,
            |thread, id| match id {
                Some(id) => match call(Control::Collect {
                    thread,
                    id,
                    cutoff_ms,
                })? {
                    Reply::Collect(allowed) => Ok(allowed),
                    _ => Err(Error::EngineUnavailable),
                },
                None => match call(Control::Probe { thread })? {
                    Reply::Thread(allowed) => Ok(allowed),
                    _ => Err(Error::EngineUnavailable),
                },
            },
            stopping,
        )
    }
}
impl GatewayState {
    /// One startup scan, sharing creation recovery's shutdown/drain lifetime.
    /// Only engine-startup unavailability retries; unsafe/failed disk entries
    /// remain for the next start, without a recurring filesystem walk.
    pub async fn recover_bots_attachments(
        &self,
        started_ms: i64,
        mut stopping: tokio::sync::watch::Receiver<bool>,
    ) {
        let Some(gateway) = self.bots_engine.get().cloned() else {
            return;
        };
        // The host captures this before installing the attachment capability,
        // not when this async task first gets polled. New uploads are excluded
        // even if task scheduling or engine startup delayed the scan.
        let mut delay = std::time::Duration::from_millis(250);
        loop {
            if *stopping.borrow() {
                return;
            }
            let permit = tokio::select! {
                _ = stopping.changed() => return,
                permit = gateway.permits.clone().acquire_owned() => { let Ok(permit) = permit else { return; }; permit }
            };
            let job = gateway.clone();
            let stop = stopping.clone();
            let result = tokio::task::spawn_blocking(move || {
                let _permit = permit;
                job.recover_attachment_files(started_ms, || *stop.borrow())
            })
            .await;
            if *stopping.borrow() {
                return;
            }
            match result {
                Ok(Ok(())) => return,
                Ok(Err(Error::EngineUnavailable)) => (),
                _ => {
                    tracing::warn!(target: "koru_bots::attachments", "startup attachment cleanup retained unavailable entries");
                    return;
                }
            }
            tokio::select! { _ = stopping.changed() => return, _ = tokio::time::sleep(delay) => {} }
            delay = (delay * 2).min(std::time::Duration::from_secs(5));
        }
    }
}
