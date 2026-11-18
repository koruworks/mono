// SPDX-License-Identifier: MPL-2.0
//! Bounded reconciliation IO outside the actor. Failed barriers/lookups remain
//! pending, including failed local commits; only exact typed replies advance.
use crate::{
    Database, Error,
    operations::{Key, State},
    workspace::{RecoveryReply, RecoveryRequest},
};
use std::{
    sync::{Arc, mpsc},
    time::{Duration, Instant},
};

type Call = Arc<dyn Fn(RecoveryRequest) -> Result<RecoveryReply, Error> + Send + Sync>;
struct Pending {
    key: Key,
    receiver: mpsc::Receiver<Result<RecoveryReply, Error>>,
    result: Option<Result<RecoveryReply, Error>>,
}
pub(super) struct Jobs {
    call: Call,
    pending: Vec<Pending>,
    after: i64,
    due: Instant,
    dirty: bool,
}
impl Jobs {
    pub(super) fn new(endpoint: koru_ipc::Endpoint, authority: crate::authority::Secret) -> Self {
        Self::with_call(Arc::new(move |request| {
            crate::workspace::recover(&endpoint, authority.clone(), request)
        }))
    }
    fn with_call(call: Call) -> Self {
        Self {
            call,
            pending: vec![],
            after: 0,
            due: Instant::now(),
            dirty: true,
        }
    }
    pub(super) fn changed(&mut self) {
        self.dirty = true;
    }
    pub(super) fn poll(&mut self, db: &mut Database, now: Instant) -> bool {
        let mut changed = false;
        self.pending.retain_mut(|pending| {
            if pending.result.is_none() {
                pending.result = match pending.receiver.try_recv() {
                    Ok(result) => Some(result),
                    Err(mpsc::TryRecvError::Empty) => None,
                    Err(mpsc::TryRecvError::Disconnected) => Some(Err(Error::EngineUnavailable)),
                };
            }
            let Some(result) = &pending.result else {
                return true;
            };
            let result = match result {
                Ok(RecoveryReply::Drained(drain)) => db
                    .record_operation_drain(&pending.key, drain.clone())
                    .map(|_| ()),
                Ok(RecoveryReply::Evidence(evidence)) => db
                    .reconcile_operation(&pending.key, evidence.clone())
                    .map(|_| ()),
                Ok(RecoveryReply::Pending) | Err(_) => return false,
            };
            match result {
                Ok(()) => {
                    changed = true;
                    false
                }
                Err(_) => true,
            }
        });
        if !self.dirty || now < self.due || self.pending.len() >= 8 {
            return changed;
        }
        self.due = now + Duration::from_secs(1);
        let mut candidates = match db.recovery_candidates(self.after, 8 - self.pending.len()) {
            Ok(rows) => rows,
            Err(_) => return changed,
        };
        if candidates.is_empty() {
            self.after = 0;
            candidates = match db.recovery_candidates(0, 8 - self.pending.len()) {
                Ok(rows) => rows,
                Err(_) => return changed,
            };
            if candidates.is_empty() {
                self.dirty = false;
                return changed;
            }
        }
        for (rowid, record, force) in candidates {
            self.after = rowid;
            if record.state != State::Leased || self.pending.iter().any(|p| p.key == record.key) {
                continue;
            }
            let key = record.key.clone();
            let request = if record.recovery.drain.is_some() {
                RecoveryRequest::Lookup { record }
            } else {
                RecoveryRequest::Fence { record, force }
            };
            let call = Arc::clone(&self.call);
            let (sender, receiver) = mpsc::sync_channel(1);
            if std::thread::Builder::new()
                .name("koru-bots reconcile".into())
                .spawn(move || {
                    let _ = sender.send(call(request));
                })
                .is_ok()
            {
                self.pending.push(Pending {
                    key,
                    receiver,
                    result: None,
                });
            }
        }
        changed
    }
}

#[cfg(test)]
mod tests;
