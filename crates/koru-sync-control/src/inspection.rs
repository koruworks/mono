// SPDX-License-Identifier: MPL-2.0

//! Bounded inspection on the existing authenticated local control session.
//! The selector conveys no authority; the host requires its live ordinary
//! Workspace session. Results never identify a global editor or Store entry.
use koru_device::cloud::CloudId;
use koru_store_wire::cloud_document::{
    CloudDocumentAction, CloudDocumentFailure, CloudDocumentSelection,
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InspectionOwner {
    pub owner_scope: u64,
    pub account_id: CloudId,
    pub account_generation: u64,
}
impl InspectionOwner {
    pub fn validate(self) -> bool {
        self.owner_scope != 0 && self.account_generation != 0 && self.account_id.is_v4()
    }
}

/// Includes JSON escaping of the bounded view and the complete control envelope.
pub const MAX_INSPECTION_RESPONSE_FRAME_BYTES: usize = 2 * 1024 * 1024 + 4096;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InspectionRequest {
    pub selection: CloudDocumentSelection,
    pub action: CloudDocumentAction,
    /// Restricts an existing ordinary writer; never grants one.
    pub can_write: bool,
}
impl std::fmt::Debug for InspectionRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("InspectionRequest(<redacted>)")
    }
}
impl InspectionRequest {
    pub fn validate(&self) -> bool {
        self.selection.validate()
            && self.action.validate()
            && (self.can_write || matches!(self.action, CloudDocumentAction::Inspect))
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum InspectionPoll {
    Pending,
    View { json: String },
    Removed,
    Failed { failure: CloudDocumentFailure },
}
impl std::fmt::Debug for InspectionPoll {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("InspectionPoll(<redacted>)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        CONTROL_FRAME_VERSION, CONTROL_PROTOCOL_VERSION, ControlHello, ControlHost, ControlRequest,
        ControlResponse, ControlSessionId, ResponseFrame,
    };
    #[test]
    fn inspection_control_rejects_old_session_invalid_owner_and_unknown_fields() {
        let hello = ControlHello {
            protocol_version: CONTROL_PROTOCOL_VERSION,
            session_id: ControlSessionId::new(),
            local_device: koru_device::DeviceId::new(),
        };
        let mut host = ControlHost::default();
        host.begin_session(&hello).unwrap();
        let owner = InspectionOwner {
            owner_scope: 1,
            account_generation: 2,
            account_id: CloudId::from_bytes(uuid::Uuid::new_v4().into_bytes()),
        };
        let token = uuid::Uuid::new_v4();
        assert!(matches!(
            host.handle(
                hello.session_id,
                ControlRequest::PollCloudInspection {
                    control_session_id: ControlSessionId::new(),
                    owner,
                    token,
                }
            ),
            ControlResponse::Rejected { .. }
        ));
        assert!(matches!(
            host.handle(
                hello.session_id,
                ControlRequest::PollCloudInspection {
                    control_session_id: hello.session_id,
                    owner: InspectionOwner {
                        owner_scope: 0,
                        ..owner
                    },
                    token,
                }
            ),
            ControlResponse::Rejected { .. }
        ));
        let mut value = serde_json::to_value(ControlRequest::PollCloudInspection {
            control_session_id: hello.session_id,
            owner,
            token,
        })
        .unwrap();
        value["extra"] = true.into();
        assert!(serde_json::from_value::<ControlRequest>(value).is_err());
    }
    #[test]
    fn maximum_escaped_view_fits_the_explicit_full_control_envelope_cap() {
        let response = ResponseFrame {
            id: u64::MAX,
            response: ControlResponse::CloudInspection(InspectionPoll::View {
                json: "\\".repeat(koru_store_wire::cloud_document::MAX_INSPECTION_BYTES),
            }),
        };
        let bytes = koru_ipc::frame::encode(CONTROL_FRAME_VERSION, &response).unwrap();
        assert!(bytes.len() <= MAX_INSPECTION_RESPONSE_FRAME_BYTES);
        assert!(!format!("{:?}", response.response).contains("\\\\"));
    }
}
