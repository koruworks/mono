// SPDX-License-Identifier: MPL-2.0

//! The phone uses the same selector-only host admission as desktop control.
use koru_account_runtime::account::{
    Cancel,
    cloud_document::{CloudDocumentContext, CloudDocumentPort},
};
use koru_device::cloud::CloudId;
use koru_store_server::cloud_inspection_host::{InspectionHub, Owner};
use koru_store_wire::cloud_document::*;
use koru_sync_control::inspection::{InspectionPoll, InspectionRequest};
use std::sync::Arc;
use std::time::{Duration, Instant};
use uuid::Uuid;

pub(super) struct Port {
    pub hub: Arc<InspectionHub>,
    pub session: Uuid,
}
struct Pending<'a> {
    hub: &'a InspectionHub,
    owner: Owner,
    token: Uuid,
}
impl Drop for Pending<'_> {
    fn drop(&mut self) {
        self.hub.cancel(self.owner, self.token);
    }
}
impl CloudDocumentPort for Port {
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
        let account = Uuid::parse_str(&selection.account_id).map_err(|_| bad)?;
        let owner = Owner {
            control_session: self.session,
            owner_scope: context.owner_scope,
            account_generation: context.account_generation,
            account: CloudId::from_bytes(account.into_bytes()),
        };
        let removing = matches!(action, CloudDocumentAction::RemoveFile { .. });
        let uncertain = if removing {
            CloudDocumentFailure::OutcomeUnknown
        } else {
            bad
        };
        let token = self.hub.start(
            owner,
            Uuid::new_v4(),
            InspectionRequest {
                selection: selection.clone(),
                action,
                can_write,
            },
        )?;
        let _pending = Pending {
            hub: &self.hub,
            owner,
            token,
        };
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if cancel.cancelled() || Instant::now() >= deadline {
                return Err(uncertain);
            }
            match self.hub.poll(owner, token) {
                InspectionPoll::Pending => std::thread::sleep(Duration::from_millis(50)),
                InspectionPoll::View { json } => {
                    let view =
                        CloudDocumentInspectionV1::from_json(&json).map_err(|_| uncertain)?;
                    if removing || view.selection != selection || cancel.cancelled() {
                        return Err(uncertain);
                    }
                    return Ok(CloudDocumentOutcome::View(view));
                }
                InspectionPoll::Removed => {
                    if !removing {
                        return Err(bad);
                    }
                    return if cancel.cancelled() {
                        Err(uncertain)
                    } else {
                        Ok(CloudDocumentOutcome::Removed)
                    };
                }
                InspectionPoll::Failed { failure } => {
                    return Err(if failure == bad { uncertain } else { failure });
                }
            }
        }
    }
}
