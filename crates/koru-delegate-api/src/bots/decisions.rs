// SPDX-License-Identifier: MPL-2.0

//! IPC16 decision vocabulary carried on the private Bot channel. These are
//! closed, strict DTOs (unknown fields refused, bounded byte fields). They are
//! typed inputs to the delegate's sole decision owner, never authority by
//! themselves: the runtime and the C ledger recheck every identity, policy and
//! revision in their own transaction.
use koru_delegate_core::decisions::{
    BotWaitId, BotWaitPhase, DecisionAnswerChoice, DecisionAnswerId, DecisionDigest,
    DecisionEditAction, DecisionId, DecisionNativeResponse, DecisionPayload, DecisionReceipt,
    DecisionResume, DecisionRevision, DecisionVersionVector,
};
use koru_delegate_core::{BotOwner, DocumentId, ProfileId, RunId};
use serde::{Deserialize, Serialize};

/// The edit a Koru decision protects, exactly as the tool call proposed it.
/// The delegate adds every Run, attempt, human and policy identity itself.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionEditProposal {
    pub target: DocumentId,
    pub action: DecisionEditAction,
    #[serde(deserialize_with = "operation_uuid")]
    pub operation_id: uuid::Uuid,
    pub edit_fingerprint: [u8; 32],
    /// The target's semantic version vector the edit was computed against.
    pub expected_revision: DecisionVersionVector,
    /// The bounded normalized edit payload. Content: never logged.
    pub payload: DecisionPayload,
}
impl std::fmt::Debug for DecisionEditProposal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("DecisionEditProposal([REDACTED])")
    }
}

/// A person's answer, submitted by the trusted human route only. The delegate
/// checks the device binding; the ledger binds it to the decision's own human,
/// exact revision and binding digest. `authority_digest` is the digest of the
/// caller's off-writer human proof and is not current permission by itself.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionAnswerRequest {
    pub owner: BotOwner,
    pub run: RunId,
    pub decision: DecisionId,
    pub expected_revision: DecisionRevision,
    pub binding_digest: DecisionDigest,
    pub request: DecisionAnswerId,
    pub human: ProfileId,
    pub choice: DecisionAnswerChoice,
    pub authority_digest: DecisionDigest,
}

/// The exact retained decision receipt after a prepare or answer, with its
/// runtime wait identity (for `ReadBotDecisionWait`). Historical evidence only.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionReceiptView {
    pub receipt: DecisionReceipt,
    pub wait: BotWaitId,
    pub wait_phase: BotWaitPhase,
    pub wait_revision: DecisionRevision,
    pub native_response: Option<DecisionNativeResponse>,
    pub resume: Option<DecisionResume>,
}

pub(super) fn operation_uuid<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<uuid::Uuid, D::Error> {
    let text = String::deserialize(d)?;
    let id = uuid::Uuid::parse_str(&text)
        .map_err(|_| serde::de::Error::custom("invalid decision operation UUID"))?;
    if id.get_version() != Some(uuid::Version::Random)
        || id.get_variant() != uuid::Variant::RFC4122
        || id.hyphenated().to_string() != text
    {
        return Err(serde::de::Error::custom("invalid decision operation UUID"));
    }
    Ok(id)
}
