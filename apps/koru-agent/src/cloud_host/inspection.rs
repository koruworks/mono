// SPDX-License-Identifier: MPL-2.0

//! Short local control requests; no wait holds the control owner or UI thread.
use crate::service::SyncControl;
use koru_account_runtime::account::{
    Cancel,
    cloud_document::{CloudDocumentContext, CloudDocumentPort},
};
use koru_device::cloud::CloudId;
use koru_store_wire::cloud_document::*;
use koru_sync_control::{
    ControlRequest, ControlResponse, ControlSessionId,
    inspection::{InspectionOwner, InspectionPoll, InspectionRequest},
};
use std::time::{Duration, Instant};
use uuid::Uuid;

pub struct DesktopCloudDocumentPort(SyncControl);
impl DesktopCloudDocumentPort {
    pub fn new(control: SyncControl) -> Self {
        Self(control)
    }
}

struct Pending<'a> {
    control: &'a SyncControl,
    session: ControlSessionId,
    owner: InspectionOwner,
    token: Uuid,
}
impl Drop for Pending<'_> {
    fn drop(&mut self) {
        // Token is known before Start, including when its acknowledgement is
        // lost. This reserved bounded lane never waits for cancellation I/O.
        let _ = self
            .control
            .inspection_request_pending(ControlRequest::CancelCloudInspection {
                control_session_id: self.session,
                owner: self.owner,
                token: self.token,
            });
    }
}
impl Pending<'_> {
    fn ask(
        &self,
        request: ControlRequest,
        cancel: &Cancel,
        deadline: Instant,
    ) -> Result<ControlResponse, ()> {
        let response = self
            .control
            .inspection_request_pending(request)
            .map_err(|_| ())?;
        loop {
            if cancel.cancelled()
                || Instant::now() >= deadline
                || self.control.control_session() != Some(self.session)
            {
                return Err(());
            }
            match response.recv_timeout(Duration::from_millis(50)) {
                Ok(Ok(response)) => return Ok(response),
                Ok(Err(_)) | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return Err(()),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            }
        }
    }
}
impl CloudDocumentPort for DesktopCloudDocumentPort {
    fn run(
        &self,
        context: CloudDocumentContext,
        selection: CloudDocumentSelection,
        action: CloudDocumentAction,
        can_write: bool,
        cancel: &Cancel,
    ) -> Result<CloudDocumentOutcome, CloudDocumentFailure> {
        let bad = CloudDocumentFailure::Unavailable;
        if cancel.cancelled() || !selection.validate() || !action.validate() {
            return Err(bad);
        }
        let session = self.0.control_session().ok_or(bad)?;
        let account = Uuid::parse_str(&selection.account_id).map_err(|_| bad)?;
        let owner = InspectionOwner {
            owner_scope: context.owner_scope,
            account_generation: context.account_generation,
            account_id: CloudId::from_bytes(account.into_bytes()),
        };
        if !owner.validate() {
            return Err(bad);
        }
        let removing = matches!(action, CloudDocumentAction::RemoveFile { .. });
        let uncertain = if removing {
            CloudDocumentFailure::OutcomeUnknown
        } else {
            bad
        };
        let deadline = Instant::now() + Duration::from_secs(20);
        let pending = Pending {
            control: &self.0,
            session,
            owner,
            token: Uuid::new_v4(),
        };
        let request = InspectionRequest {
            selection: selection.clone(),
            action,
            can_write,
        };
        if !request.validate() {
            return Err(bad);
        }
        match pending
            .ask(
                ControlRequest::StartCloudInspection {
                    control_session_id: session,
                    owner,
                    token: pending.token,
                    request: Box::new(request),
                },
                cancel,
                deadline,
            )
            .map_err(|_| uncertain)?
        {
            ControlResponse::CloudInspectionStarted { token } if token == pending.token => {}
            ControlResponse::CloudInspection(InspectionPoll::Failed { failure }) => {
                return Err(failure);
            }
            _ => return Err(uncertain),
        }
        loop {
            if cancel.cancelled() {
                return Err(uncertain);
            }
            let response = pending
                .ask(
                    ControlRequest::PollCloudInspection {
                        control_session_id: session,
                        owner,
                        token: pending.token,
                    },
                    cancel,
                    deadline,
                )
                .map_err(|_| uncertain)?;
            if cancel.cancelled() || self.0.control_session() != Some(session) {
                return Err(uncertain);
            }
            match response {
                ControlResponse::CloudInspection(InspectionPoll::Pending) => {
                    std::thread::sleep(Duration::from_millis(50))
                }
                ControlResponse::CloudInspection(InspectionPoll::View { json }) => {
                    let view =
                        CloudDocumentInspectionV1::from_json(&json).map_err(|_| uncertain)?;
                    if removing || view.selection != selection {
                        return Err(uncertain);
                    }
                    return Ok(CloudDocumentOutcome::View(view));
                }
                ControlResponse::CloudInspection(InspectionPoll::Removed) => {
                    if !removing {
                        return Err(bad);
                    }
                    return Ok(CloudDocumentOutcome::Removed);
                }
                ControlResponse::CloudInspection(InspectionPoll::Failed { failure }) => {
                    return Err(if failure == bad { uncertain } else { failure });
                }
                _ => return Err(uncertain),
            }
        }
    }
}
