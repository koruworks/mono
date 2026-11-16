// SPDX-License-Identifier: MPL-2.0

//! Cohort `m5_policy_bytes`, and the private vocabulary's own shape.
use super::*;
use crate::decisions::DecisionDigest;
use crate::{BotAccess, CapturedBotPolicy, WorkspaceId};
use std::collections::BTreeSet;

fn uuid(n: u32) -> Uuid {
    format!("00000000-0000-4000-8000-{n:012x}").parse().unwrap()
}
fn policy() -> CapturedBotPolicy {
    let workspace = WorkspaceId::from_uuid(uuid(10));
    CapturedBotPolicy {
        workspace,
        access: BotAccess::Default,
        selected_pages: BTreeSet::new(),
        destination: workspace,
        connections: BTreeSet::new(),
    }
}
fn grant(n: u32) -> ConnectionGrantRef {
    ConnectionGrantRef {
        connection: ConnectionId::from_uuid(uuid(n)),
        operation: PrivateOperationKind::ReadItem,
    }
}
fn proposal() -> PrivateOperationProposal {
    let connection = ConnectionId::from_uuid(uuid(40));
    let item = ConnectionItemId::from_uuid(uuid(41));
    let item_ref = DecisionDigest::from_bytes([7; 32]);
    PrivateOperationProposal {
        operation_id: uuid(12),
        connection,
        connection_revision: DecisionRevision::new(3).unwrap(),
        account: DecisionDigest::from_bytes([6; 32]),
        item,
        item_ref,
        operation: PrivateOperationKind::ReadItem,
        request_fingerprint: private_request_fingerprint(
            PrivateOperationKind::ReadItem,
            connection,
            item,
            item_ref,
        ),
    }
}

// The bytes a policy had before M5 added `connections`, authored as a literal
// and never produced by the serializer under test.
const PRE_M5_POLICY: &str = r#"{"workspace":"00000000-0000-4000-8000-00000000000a","access":"default","selected_pages":[],"destination":"00000000-0000-4000-8000-00000000000a"}"#;
const ONE_GRANT_POLICY: &str = r#"{"workspace":"00000000-0000-4000-8000-00000000000a","access":"default","selected_pages":[],"destination":"00000000-0000-4000-8000-00000000000a","connections":[{"connection":"00000000-0000-4000-8000-000000000028","operation":"read_item"}]}"#;

#[test]
fn m5_policy_bytes_without_a_grant_are_the_pre_m5_bytes_and_digest() {
    let encoded = serde_json::to_string(&policy()).unwrap();
    assert_eq!(encoded, PRE_M5_POLICY);
    // Any digest is a function of these bytes, so equal bytes are an equal
    // digest under every domain that hashes a policy.
    assert_eq!(
        blake3::hash(encoded.as_bytes()),
        blake3::hash(PRE_M5_POLICY.as_bytes())
    );
    // A pre-M5 encoding, which has no such field, still decodes.
    assert_eq!(
        serde_json::from_str::<CapturedBotPolicy>(PRE_M5_POLICY).unwrap(),
        policy()
    );
}

#[test]
fn m5_policy_bytes_with_one_grant_are_pinned_and_roundtrip() {
    let mut p = policy();
    p.connections.insert(grant(40));
    let encoded = serde_json::to_string(&p).unwrap();
    assert_eq!(encoded, ONE_GRANT_POLICY);
    assert_eq!(
        serde_json::from_str::<CapturedBotPolicy>(&encoded).unwrap(),
        p
    );
    assert!(p.valid() && p.grants_connection(&grant(40)));
    assert!(!p.grants_connection(&grant(41)));
    assert!(!policy().grants_connection(&grant(40)));
}

#[test]
fn m5_policy_bytes_bound_grants_and_refuse_unknown_fields() {
    let mut p = policy();
    for n in 0..CONNECTION_GRANT_LIMIT as u32 {
        p.connections.insert(grant(100 + n));
    }
    assert!(p.valid());
    p.connections.insert(grant(999));
    assert_eq!(p.connections.len(), 17);
    assert!(!p.valid());

    let unknown = PRE_M5_POLICY.replacen(
        "\"access\":\"default\"",
        "\"access\":\"default\",\"browser\":true",
        1,
    );
    assert!(serde_json::from_str::<CapturedBotPolicy>(&unknown).is_err());
    let unknown_grant = ONE_GRANT_POLICY.replacen(
        "\"operation\":\"read_item\"",
        "\"operation\":\"read_item\",\"label\":\"x\"",
        1,
    );
    assert!(serde_json::from_str::<CapturedBotPolicy>(&unknown_grant).is_err());
    let unknown_operation = ONE_GRANT_POLICY.replacen("read_item", "write_item", 1);
    assert!(serde_json::from_str::<CapturedBotPolicy>(&unknown_operation).is_err());
}

#[test]
fn m5_policy_bytes_order_grants_by_connection_then_operation() {
    let mut p = policy();
    p.connections.insert(grant(42));
    p.connections.insert(grant(40));
    p.connections.insert(grant(41));
    let order: Vec<_> = p.connections.iter().map(|g| g.connection).collect();
    assert_eq!(
        order,
        [40, 41, 42]
            .map(|n| ConnectionId::from_uuid(uuid(n)))
            .to_vec()
    );
}

// The fingerprint's preimage, authored as a literal.
const FINGERPRINT_PREIMAGE: &str = r#"{"version":1,"operation":"read_item","connection":"00000000-0000-4000-8000-000000000028","item":"00000000-0000-4000-8000-000000000029","item_ref":"0707070707070707070707070707070707070707070707070707070707070707"}"#;

#[test]
fn m5_private_binding_fingerprint_has_a_fixed_domain_and_record() {
    let p = proposal();
    let mut reference = blake3::Hasher::new();
    reference.update(b"koru.bot.private.request.v1");
    reference.update(FINGERPRINT_PREIMAGE.as_bytes());
    assert_eq!(&p.request_fingerprint, reference.finalize().as_bytes());
    assert!(p.valid());

    // It excludes the operation id and the connection revision.
    let mut again = p.clone();
    again.operation_id = uuid(13);
    again.connection_revision = DecisionRevision::new(9).unwrap();
    assert_eq!(again.fingerprint(), p.request_fingerprint);
    assert!(again.valid());

    // It binds the connection, the item and the item reference.
    let mut other = p.clone();
    other.item = ConnectionItemId::from_uuid(uuid(77));
    assert!(!other.valid());
    let mut other = p.clone();
    other.connection = ConnectionId::from_uuid(uuid(77));
    assert!(!other.valid());
    let mut other = p.clone();
    other.item_ref = DecisionDigest::from_bytes([8; 32]);
    assert!(!other.valid());
    // A caller's assertion is not accepted.
    let mut asserted = p.clone();
    asserted.request_fingerprint = [1; 32];
    assert!(!asserted.valid());
    // The operation id must be a v4 UUID.
    let mut nil = p;
    nil.operation_id = Uuid::nil();
    assert!(!nil.valid());
}

#[test]
fn m5_private_vocabulary_is_strict_and_uses_the_sql_tokens() {
    let p = proposal();
    let value = serde_json::to_value(&p).unwrap();
    assert_eq!(
        serde_json::from_value::<PrivateOperationProposal>(value.clone()).unwrap(),
        p
    );
    let mut extra = value.clone();
    extra["title"] = "x".into();
    assert!(serde_json::from_value::<PrivateOperationProposal>(extra).is_err());
    let mut missing = value;
    missing.as_object_mut().unwrap().remove("account");
    assert!(serde_json::from_value::<PrivateOperationProposal>(missing).is_err());

    let refusals = [
        (PrivateEffectRefusal::NotDispatched, "not_dispatched"),
        (PrivateEffectRefusal::AccessDenied, "access_denied"),
        (PrivateEffectRefusal::Revoked, "revoked"),
        (PrivateEffectRefusal::WrongAccount, "wrong_account"),
        (
            PrivateEffectRefusal::CredentialExpired,
            "credential_expired",
        ),
        (PrivateEffectRefusal::CredentialLocked, "credential_locked"),
        (
            PrivateEffectRefusal::CredentialMissing,
            "credential_missing",
        ),
        (PrivateEffectRefusal::ItemUnavailable, "item_unavailable"),
        (
            PrivateEffectRefusal::DestinationChanged,
            "destination_changed",
        ),
        (PrivateEffectRefusal::VendorRejected, "vendor_rejected"),
        (PrivateEffectRefusal::Unavailable, "unavailable"),
    ];
    for (code, token) in refusals {
        assert_eq!(serde_json::to_value(code).unwrap(), token);
    }
    let stages = [
        (PrivateEffectStage::Prepared, "prepared", false),
        (PrivateEffectStage::Dispatched, "dispatched", false),
        (PrivateEffectStage::Confirmed, "confirmed", true),
        (PrivateEffectStage::Refused, "refused", true),
        (PrivateEffectStage::Unknown, "unknown", true),
    ];
    for (stage, token, last) in stages {
        assert_eq!(serde_json::to_value(stage).unwrap(), token);
        assert_eq!(stage.is_final(), last);
    }

    let transition = PrivateEffectTransition::Confirmed {
        outcome_bytes: 5,
        withheld: false,
    };
    assert_eq!(
        serde_json::to_string(&transition).unwrap(),
        r#"{"transition":"confirmed","outcome_bytes":5,"withheld":false}"#
    );
    assert!(
        serde_json::from_str::<PrivateEffectTransition>(
            r#"{"transition":"dispatched","content":"x"}"#
        )
        .is_err()
    );
    assert_eq!(
        serde_json::from_str::<PrivateEffectTransition>(r#"{"transition":"unknown"}"#).unwrap(),
        PrivateEffectTransition::Unknown {}
    );
}

#[test]
fn m5_private_proposal_debug_names_no_connection_account_or_item() {
    let p = proposal();
    let debug = format!("{p:?}");
    for hidden in [
        p.connection.to_storage(),
        p.item.to_storage(),
        String::from(p.account),
        String::from(p.item_ref),
    ] {
        assert!(!debug.contains(&hidden));
    }
}
