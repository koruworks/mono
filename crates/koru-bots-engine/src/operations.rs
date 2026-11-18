// SPDX-License-Identifier: MPL-2.0

//! Engine-owned native intent and one execution lease. Document semantics and
//! permanent fences remain with the existing native MCP and Store owners.
pub mod control;
mod ledger;
mod outcomes;
pub(crate) mod outputs;
mod recovery;
use crate::{Error, authority::Binding};
use koru_delegate_core::NativeResultReference;
use koru_store_core::{
    DocumentId, NativeEffectBinding, NativeEffectDrainReceipt, NativeEffectKind, WorkspaceId,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Key {
    pub scope: String,
    pub bot: Uuid,
    pub thread: Uuid,
    pub operation: Uuid,
}

/// Supplied by the gateway's existing semantic builder, before dispatch.
/// fingerprint is that builder's semantic fingerprint, not a transport hash.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Prepared {
    pub kind: NativeEffectKind,
    pub workspace: WorkspaceId,
    pub document: Option<DocumentId>,
    pub fingerprint: [u8; 32],
    pub expected_revision: Option<Vec<u8>>,
    pub title: String,
}
impl Prepared {
    fn validate(&self) -> Result<(), Error> {
        if self.workspace.as_uuid().is_nil() || self.title.len() > 256 {
            return Err(Error::InvalidRequest);
        }
        match self.kind {
            NativeEffectKind::CreateDocument
                if self.document.is_none() && self.expected_revision.is_none() =>
            {
                Ok(())
            }
            NativeEffectKind::EditDocument
                if self.document.is_some_and(|id| !id.as_uuid().is_nil()) =>
            {
                let revision = self
                    .expected_revision
                    .as_ref()
                    .ok_or(Error::InvalidRequest)?;
                koru_store_core::native_effect::same_native_edit_revision(revision, revision)
                    .map_err(|_| Error::InvalidRequest)?;
                Ok(())
            }
            _ => Err(Error::InvalidRequest),
        }
    }
    /// Compare semantic intent, decoding revision vectors rather than their
    /// incidental serialized map ordering.
    pub fn same(&self, other: &Self) -> bool {
        let mut normalized = other.clone();
        normalized.expected_revision = self.expected_revision.clone();
        self == &normalized
            && match (&self.expected_revision, &other.expected_revision) {
                (None, None) => true,
                (Some(a), Some(b)) => {
                    koru_store_core::native_effect::same_native_edit_revision(a, b).unwrap_or(false)
                }
                _ => false,
            }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Recorded,
    Leased,
    Applied,
    Refused,
    Unresolved,
}
impl State {
    fn parse(token: &str) -> Result<Self, Error> {
        match token {
            "recorded" => Ok(Self::Recorded),
            "leased" => Ok(Self::Leased),
            "applied" => Ok(Self::Applied),
            "refused" => Ok(Self::Refused),
            "unresolved" => Ok(Self::Unresolved),
            _ => Err(Error::StorageUnavailable),
        }
    }
}
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lease {
    pub id: Uuid,
    pub boundary: Uuid,
    pub acquire_command: Uuid,
    pub binding: Binding,
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Outcome {
    Applied {
        evidence: NativeResultReference,
        result: Value,
    },
    /// Only the trusted lease holder can attest a refusal before dispatch.
    Refused { stable_code: String },
}
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Recovery {
    pub drain: Option<NativeEffectDrainReceipt>,
    pub continued_by: Option<Uuid>,
    #[serde(default)]
    pub observations: Vec<Outcome>,
    #[serde(default)]
    pub requested: bool,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Record {
    pub key: Key,
    pub prepared: Prepared,
    pub store: NativeEffectBinding,
    pub output: Uuid,
    pub recorded_call: Binding,
    pub state: State,
    pub lease: Option<Lease>,
    pub outcome: Option<Outcome>,
    pub recovery: Recovery,
}

/// Execute is returned only on the transaction that consumes the sole lease.
/// Receipts retain consumption; no retry replays this capability.
#[derive(Clone, Serialize, Deserialize)]
pub enum Admission {
    Execute(Record),
    Pending(Record),
    Terminal(Record),
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Continued {
    pub messages: Vec<Uuid>,
}

#[derive(Serialize, Deserialize)]
struct Immutable {
    prepared: Prepared,
    store: NativeEffectBinding,
    output: Uuid,
    recorded_call: Binding,
}

fn v4(id: Uuid) -> bool {
    id.get_version_num() == 4 && id.get_variant() == uuid::Variant::RFC4122
}
fn encode(value: &impl Serialize) -> Result<Vec<u8>, Error> {
    serde_json::to_vec(value).map_err(|_| Error::InvalidRequest)
}
fn decode<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<T, Error> {
    serde_json::from_slice(bytes).map_err(|_| Error::StorageUnavailable)
}

#[cfg(test)]
pub(crate) mod tests;
