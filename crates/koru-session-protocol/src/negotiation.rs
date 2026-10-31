// SPDX-License-Identifier: MPL-2.0

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{CapabilitySet, ErrorCode, SessionFailure};

pub const MIN_PROTOCOL_VERSION: u32 = 1;
/// v10 adds the project verbs and the named catalog subscription. New
/// *variants* need a version, not a `#[serde(default)]`: a defaulted field is
/// readable by an older peer, an unknown tag is not.
///
/// v11 adds the supervised workspace-lifecycle verb — one request tag, one
/// response tag — for the same reason: an older host handed
/// `terminate_workspace_sessions` would answer that the request is
/// unreadable, and "unreadable" and "I closed nothing" are not the same
/// answer to a caller about to delete a workspace.
/// v12 adds the supervised project-rescope verb — again one request tag and
/// one response tag, and again because an unknown tag is unreadable rather
/// than ignorable. It rides the same `workspace-lifecycle` capability as v11
/// rather than minting a second one: the capability names an authority family,
/// not a verb, and no peer would ever want to close a workspace's terminals
/// while refusing to let its projects follow the workspace. The version is
/// what tells a v11 host apart from a v12 one; the capability is what says the
/// peer speaks this family at all.
/// v13 adds the per-project scope rewrite — one request tag answering with the
/// project row, for the same reason as every bump above it: an unknown tag is
/// unreadable, not ignorable. It rides the existing `projects` capability
/// rather than a new one, because that capability already names the whole
/// project family — its verbs, its catalog subscription and its placement at
/// create — and a peer that may rename, re-root and delete a project has no
/// reason to be refused the one field it could not write. The version is what
/// tells a v12 host apart from a v13 one.
///
/// v14 adds the delegation family behind its own `delegation` capability:
/// the **non-creating creation lookup** and the **retirement** of a creation
/// key (`lookup_creation`, `retire_creation`), the **controller handover**
/// verbs (`request_control`, `release_control`, `return_control`), and one
/// new session-event tag (`handover_changed`). Each is a new tag, so each
/// needs the version. The family also carries defaulted *fields* that needed
/// no bump on their own — `cancel_turn.reason`, `AgentUsage.model`,
/// `AgentUsage.cached_input`, `SessionSummary.handover`,
/// `SessionSummary.model`, `CreateProfileSession.delegation` and
/// `DelegationLaunch.credential` (the run endpoint's bearer, added with the
/// authority that issues it) — and rides
/// them here so one number says whether a peer can be delegated to at all. A
/// peer negotiating 13 has no lookup, and without the lookup the recovery
/// guarantee cannot hold, so managed delegation refuses it at admission; its
/// manual managed profiles are untouched.
/// v15 adds negotiated provider model discovery and explicit launch selection.
///
/// v16 adds **session origin ownership** behind the `session-origin`
/// capability — the `lookup_origin` request and its `origin_lookup` answer
/// (new tags, so the version), the `ai_control` hello role that mints a
/// creation generation, and the defaulted `origin` field on a creation and
/// on a summary — and **explicit launch selection** behind
/// `launch-selection`: the defaulted `selection` on a creation. Both fields
/// are readable by an older host, which is exactly the hazard: an older
/// host would ignore them and run the provider's default, so a client
/// checks the negotiated capability before sending either and refuses to
/// start a chat session against a host that cannot own it.
///
/// v18 adds what an agent says about each model it lists — its name,
/// description, group name, legacy mark and reasoning levels — as optional
/// fields on `ProviderModel`, and the `xhigh` reasoning token.
pub const MAX_PROTOCOL_VERSION: u32 = 18;
pub const PROVIDER_MODELS_PROTOCOL_VERSION: u32 = 15;
/// The first version whose tool events may carry the tool's own words: a
/// failed call's message (`ToolCompleted.detail`), what the call was asked
/// (`ToolStarted.input`) and what it produced (`ToolCompleted.output`).
/// Optional fields an older peer leaves unread: no capability, the number
/// alone.
pub const TOOL_DETAIL_PROTOCOL_VERSION: u32 = 17;
/// The first version whose provider models may carry the agent's own facts
/// about each model, and whose launch selection may carry `xhigh`. The
/// fields are optional and an older peer leaves them unread: no capability,
/// the number alone. The token is not, so `xhigh` goes only to a v18 host.
pub const PROVIDER_MODEL_FACTS_PROTOCOL_VERSION: u32 = 18;
/// The first version that can carry the session-origin family's tags.
pub const SESSION_ORIGIN_PROTOCOL_VERSION: u32 = 16;
/// The first version that can carry the delegation family's tags.
pub const DELEGATION_PROTOCOL_VERSION: u32 = 14;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ProtocolRange {
    pub min: u32,
    pub max: u32,
}

impl ProtocolRange {
    pub fn validate(self) -> Result<(), ErrorCode> {
        (self.min > 0 && self.min <= self.max)
            .then_some(())
            .ok_or(ErrorCode::InvalidRequest)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ProtocolOffer {
    pub versions: ProtocolRange,
    #[serde(default)]
    pub capabilities: CapabilitySet,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ProtocolAgreement {
    pub version: u32,
    pub capabilities: CapabilitySet,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ProtocolRefusal {
    pub client: ProtocolRange,
    pub server: ProtocolRange,
    pub error: SessionFailure,
}

pub fn negotiate(
    client: &ProtocolOffer,
    server: &ProtocolOffer,
) -> Result<ProtocolAgreement, ProtocolRefusal> {
    if client.versions.validate().is_err() || server.versions.validate().is_err() {
        return Err(refusal(
            client.versions,
            server.versions,
            "invalid version range",
        ));
    }
    let min = client.versions.min.max(server.versions.min);
    let max = client.versions.max.min(server.versions.max);
    if min > max {
        return Err(refusal(
            client.versions,
            server.versions,
            "protocol ranges do not overlap",
        ));
    }
    Ok(ProtocolAgreement {
        version: max,
        capabilities: client.capabilities.intersection(&server.capabilities),
    })
}

fn refusal(client: ProtocolRange, server: ProtocolRange, message: &str) -> ProtocolRefusal {
    ProtocolRefusal {
        client,
        server,
        error: SessionFailure {
            code: ErrorCode::ProtocolIncompatible,
            message: message.to_owned(),
        },
    }
}
